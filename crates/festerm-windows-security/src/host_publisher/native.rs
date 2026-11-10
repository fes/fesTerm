use super::{admit, approved_subject, refused, Evidence, Revocation};
use std::{
    fs::File,
    io,
    mem::size_of,
    os::windows::{ffi::OsStrExt, io::AsRawHandle},
    path::Path,
    ptr,
};
use windows_sys::Win32::{
    Foundation::INVALID_HANDLE_VALUE,
    Security::{Cryptography::*, WinTrust::*},
};

struct TrustState(WINTRUST_DATA);

impl Drop for TrustState {
    fn drop(&mut self) {
        self.0.dwStateAction = WTD_STATEACTION_CLOSE;
        // The file/path and all provider data remain live until this closes.
        unsafe {
            let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
            WinVerifyTrust(
                INVALID_HANDLE_VALUE,
                &mut action,
                (&mut self.0 as *mut WINTRUST_DATA).cast(),
            );
        }
    }
}

/// Verify the retained private file handle, not a separately reopened pathname.
/// Online retrieval is enabled; unavailable/stale/unknown revocation refuses.
pub fn verify(file: &File, path: &Path) -> io::Result<()> {
    let path: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    if path.len() > 32768 || path[..path.len() - 1].contains(&0) {
        return Err(refused());
    }
    let mut input = WINTRUST_FILE_INFO {
        cbStruct: size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: path.as_ptr(),
        hFile: file.as_raw_handle(),
        ..Default::default()
    };
    let mut state = TrustState(WINTRUST_DATA {
        cbStruct: size_of::<WINTRUST_DATA>() as u32,
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_WHOLECHAIN,
        dwUnionChoice: WTD_CHOICE_FILE,
        Anonymous: WINTRUST_DATA_0 { pFile: &mut input },
        dwStateAction: WTD_STATEACTION_VERIFY,
        // Deliberately no CACHE_ONLY, REVOCATION_NONE or LIFETIME_SIGNING flag.
        dwProvFlags: WTD_REVOCATION_CHECK_CHAIN_EXCLUDE_ROOT | WTD_DISABLE_MD2_MD4,
        ..Default::default()
    });
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    let status = unsafe {
        WinVerifyTrust(
            INVALID_HANDLE_VALUE,
            &mut action,
            (&mut state.0 as *mut WINTRUST_DATA).cast(),
        )
    };
    if status != 0 || state.0.hWVTStateData.is_null() {
        return Err(refused());
    }
    // Provider pointers are borrowed exclusively from the live WinTrust state.
    unsafe {
        let provider = WTHelperProvDataFromStateData(state.0.hWVTStateData)
            .as_ref()
            .ok_or_else(refused)?;
        if provider.dwError != 0 || provider.csSigners != 1 {
            return Err(refused());
        }
        let signer = WTHelperGetProvSignerFromChain(
            provider as *const CRYPT_PROVIDER_DATA as *mut CRYPT_PROVIDER_DATA,
            0,
            0,
            0,
        )
        .as_ref()
        .ok_or_else(refused)?;
        if signer.dwError != 0
            || signer.csCertChain != 4
            || signer.pasCertChain.is_null()
            || signer.csCounterSigners != 1
            || signer.pasCounterSigners.is_null()
            || signer.pChainContext.is_null()
            || (*signer.pChainContext).TrustStatus.dwErrorStatus != 0
        {
            return Err(refused());
        }
        let certificates = std::slice::from_raw_parts(signer.pasCertChain, 4);
        for certificate in certificates {
            if certificate.pCert.is_null()
                || (*certificate.pCert).pCertInfo.is_null()
                || certificate.dwError != 0
                || certificate.fTestCert != 0
            {
                return Err(refused());
            }
        }
        let leaf = certificates[0].pCert;
        let chain = certificates[1..]
            .iter()
            .map(|certificate| fingerprint(certificate.pCert))
            .collect::<io::Result<Vec<_>>>()?;
        let ekus = enhanced_usages(leaf)?;
        let timestamp = &*signer.pasCounterSigners;
        let trusted_timestamp = timestamp.dwError == 0
            && !timestamp.pChainContext.is_null()
            && (*timestamp.pChainContext).TrustStatus.dwErrorStatus == 0
            && timestamp.csCertChain > 0
            && timestamp.csCertChain <= 8
            && !timestamp.pasCertChain.is_null()
            && rfc3161_timestamp(signer, timestamp, leaf)?;
        if !trusted_timestamp {
            return Err(refused());
        }
        let timestamp_chain =
            std::slice::from_raw_parts(timestamp.pasCertChain, timestamp.csCertChain as usize);
        let mut revocation = fresh_revocation(certificates)?;
        if revocation == Revocation::FreshGood {
            revocation = fresh_revocation(timestamp_chain)?;
        }
        admit(&Evidence {
            subject_matches: subject_matches(leaf)?,
            ekus: &ekus,
            chain: &chain,
            native_signature_and_digest: true,
            trusted_rfc3161: trusted_timestamp,
            revocation,
        })
    }
}

fn fresh_revocation(chain: &[CRYPT_PROVIDER_CERT]) -> io::Result<Revocation> {
    if !(2..=8).contains(&chain.len()) || chain.last().is_none_or(|cert| cert.fTrustedRoot == 0) {
        return Err(refused());
    }
    for certificate in chain {
        if certificate.pCert.is_null() || certificate.dwError != 0 || certificate.fTestCert != 0 {
            return Err(refused());
        }
    }
    for pair in chain.windows(2) {
        let parameters = CERT_REVOCATION_PARA {
            cbSize: size_of::<CERT_REVOCATION_PARA>() as u32,
            pIssuerCert: pair[1].pCert,
            // Current time, OS CRL/OCSP NextUpdate, online retrieval enabled.
            // No historical signing-time revocation or custom offline grace.
            ..Default::default()
        };
        let mut status = CERT_REVOCATION_STATUS {
            cbSize: size_of::<CERT_REVOCATION_STATUS>() as u32,
            ..Default::default()
        };
        let context = pair[0].pCert.cast();
        let verified = unsafe {
            CertVerifyRevocation(
                X509_ASN_ENCODING,
                CERT_CONTEXT_REVOCATION_TYPE,
                1,
                &context,
                0,
                &parameters,
                &mut status,
            )
        };
        if verified == 0 || status.dwError != 0 {
            return Ok(match status.dwError {
                0x80092010 | 0x800b010c => Revocation::Revoked,
                0x80092013 => Revocation::Stale,
                _ => Revocation::Unknown,
            });
        }
    }
    Ok(Revocation::FreshGood)
}

fn fingerprint(certificate: *const CERT_CONTEXT) -> io::Result<[u8; 32]> {
    let mut bytes = [0; 32];
    let mut length = bytes.len() as u32;
    if unsafe {
        CertGetCertificateContextProperty(
            certificate,
            CERT_SHA256_HASH_PROP_ID,
            bytes.as_mut_ptr().cast(),
            &mut length,
        )
    } == 0
        || length != 32
    {
        return Err(refused());
    }
    Ok(bytes)
}

fn subject_matches(certificate: *const CERT_CONTEXT) -> io::Result<bool> {
    let subject = unsafe { &(*(*certificate).pCertInfo).Subject };
    if subject.pbData.is_null() || subject.cbData == 0 || subject.cbData > 64 * 1024 {
        return Err(refused());
    }
    let mut length = 0;
    if unsafe {
        CryptDecodeObjectEx(
            X509_ASN_ENCODING,
            X509_NAME,
            subject.pbData,
            subject.cbData,
            0,
            ptr::null(),
            ptr::null_mut(),
            &mut length,
        )
    } == 0
        || !(size_of::<CERT_NAME_INFO>()..=64 * 1024).contains(&(length as usize))
    {
        return Err(refused());
    }
    let mut buffer = vec![0usize; (length as usize).div_ceil(size_of::<usize>())];
    let name = buffer.as_mut_ptr().cast::<CERT_NAME_INFO>();
    let capacity = length;
    if unsafe {
        CryptDecodeObjectEx(
            X509_ASN_ENCODING,
            X509_NAME,
            subject.pbData,
            subject.cbData,
            0,
            ptr::null(),
            name.cast(),
            &mut length,
        )
    } == 0
        || length > capacity
    {
        return Err(refused());
    }
    unsafe {
        if (*name).cRDN != 5 || (*name).rgRDN.is_null() {
            return Ok(false);
        }
        let mut fields = Vec::new();
        for rdn in std::slice::from_raw_parts((*name).rgRDN, 5) {
            if rdn.cRDNAttr != 1 || rdn.rgRDNAttr.is_null() {
                return Ok(false);
            }
            let attribute = &*rdn.rgRDNAttr;
            let mut value = [0u16; 256];
            let count = CertRDNValueToStrW(
                attribute.dwValueType,
                &attribute.Value,
                value.as_mut_ptr(),
                value.len() as u32,
            ) as usize;
            if count == 0 || count >= value.len() || value[count - 1] != 0 {
                return Err(refused());
            }
            if attribute.Value.pbData.is_null() || attribute.Value.cbData > 1024 {
                return Err(refused());
            }
            let decoded = String::from_utf16(&value[..count - 1]).map_err(|_| refused())?;
            let raw =
                std::slice::from_raw_parts(attribute.Value.pbData, attribute.Value.cbData as usize);
            let unicode: Vec<_> = decoded.encode_utf16().flat_map(u16::to_le_bytes).collect();
            // All approved fields are ASCII. Reject hidden/truncated content
            // even if native string formatting were to stop at an inner NUL.
            if raw != decoded.as_bytes() && raw != unicode {
                return Ok(false);
            }
            fields.push((oid_text(attribute.pszObjId)?, decoded));
        }
        // Compare decoded ASN.1 string values, not an encoding-dependent DER
        // spelling; exact order/count prevents extra or duplicate RDNs.
        Ok(approved_subject(&fields))
    }
}

fn enhanced_usages(certificate: *const CERT_CONTEXT) -> io::Result<Vec<String>> {
    let mut length = 0;
    if unsafe {
        CertGetEnhancedKeyUsage(
            certificate,
            CERT_FIND_EXT_ONLY_ENHKEY_USAGE_FLAG,
            ptr::null_mut(),
            &mut length,
        )
    } == 0
        || !(size_of::<CTL_USAGE>()..=64 * 1024).contains(&(length as usize))
    {
        return Err(refused());
    }
    let mut buffer = vec![0usize; (length as usize).div_ceil(size_of::<usize>())];
    let usage = buffer.as_mut_ptr().cast::<CTL_USAGE>();
    let capacity = length;
    if unsafe {
        CertGetEnhancedKeyUsage(
            certificate,
            CERT_FIND_EXT_ONLY_ENHKEY_USAGE_FLAG,
            usage,
            &mut length,
        )
    } == 0
        || length > capacity
    {
        return Err(refused());
    }
    unsafe {
        if (*usage).cUsageIdentifier > 64 || (*usage).rgpszUsageIdentifier.is_null() {
            return Err(refused());
        }
        std::slice::from_raw_parts(
            (*usage).rgpszUsageIdentifier,
            (*usage).cUsageIdentifier as usize,
        )
        .iter()
        .map(|oid| oid_text(*oid))
        .collect()
    }
}

fn oid_text(oid: *const u8) -> io::Result<String> {
    if oid.is_null() {
        return Err(refused());
    }
    let mut bytes = Vec::new();
    for offset in 0..=128 {
        // CryptoAPI owns the NUL-terminated OID for the retained state/buffer.
        let byte = unsafe { *oid.add(offset) };
        if byte == 0 {
            return String::from_utf8(bytes).map_err(|_| refused());
        }
        bytes.push(byte);
    }
    Err(refused())
}

struct TimestampProof {
    context: *mut CRYPT_TIMESTAMP_CONTEXT,
    signer: *mut CERT_CONTEXT,
    store: HCERTSTORE,
}

impl Drop for TimestampProof {
    fn drop(&mut self) {
        unsafe {
            if !self.context.is_null() {
                CryptMemFree(self.context.cast());
            }
            if !self.signer.is_null() {
                CertFreeCertificateContext(self.signer);
            }
            if !self.store.is_null() {
                CertCloseStore(self.store, 0);
            }
        }
    }
}

fn rfc3161_timestamp(
    provider_signer: &CRYPT_PROVIDER_SGNR,
    timestamp: &CRYPT_PROVIDER_SGNR,
    leaf: *const CERT_CONTEXT,
) -> io::Result<bool> {
    unsafe {
        let signer = provider_signer.psSigner.as_ref().ok_or_else(refused)?;
        let attributes = &signer.UnauthAttrs;
        if attributes.cAttr == 0 || attributes.cAttr > 32 || attributes.rgAttr.is_null() {
            return Ok(false);
        }
        let mut content = None;
        for attribute in std::slice::from_raw_parts(attributes.rgAttr, attributes.cAttr as usize) {
            if oid_text(attribute.pszObjId)? == "1.3.6.1.4.1.311.3.3.1" {
                if content.is_some() || attribute.cValue != 1 || attribute.rgValue.is_null() {
                    return Ok(false);
                }
                content = Some(&*attribute.rgValue);
            }
        }
        let Some(content) = content else {
            return Ok(false);
        };
        if content.pbData.is_null()
            || content.cbData == 0
            || content.cbData > 1024 * 1024
            || signer.EncryptedHash.pbData.is_null()
            || signer.EncryptedHash.cbData == 0
            || signer.EncryptedHash.cbData > 16 * 1024
        {
            return Ok(false);
        }
        let mut proof = TimestampProof {
            context: ptr::null_mut(),
            signer: ptr::null_mut(),
            store: ptr::null_mut(),
        };
        // Authenticate the RFC3161 message imprint over this signature, not
        // just the presence of an unauthenticated timestamp attribute.
        if CryptVerifyTimeStampSignature(
            content.pbData,
            content.cbData,
            signer.EncryptedHash.pbData,
            signer.EncryptedHash.cbData,
            ptr::null_mut(),
            &mut proof.context,
            &mut proof.signer,
            &mut proof.store,
        ) == 0
            || proof.context.is_null()
            || proof.signer.is_null()
            || (*proof.context).pTimeStamp.is_null()
        {
            return Ok(false);
        }
        let time = (*(*proof.context).pTimeStamp).ftTime;
        let trusted_signer = (*timestamp.pasCertChain).pCert;
        Ok(!trusted_signer.is_null()
            && fingerprint(trusted_signer)? == fingerprint(proof.signer)?
            && time.dwLowDateTime == provider_signer.sftVerifyAsOf.dwLowDateTime
            && time.dwHighDateTime == provider_signer.sftVerifyAsOf.dwHighDateTime
            && CertVerifyTimeValidity(&time, (*leaf).pCertInfo) == 0)
    }
}
