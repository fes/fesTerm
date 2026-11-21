use super::{
    compatible_identities, create_if_missing, input, policy, update_selected, Error, MAX_ACLS,
    MAX_AUTHORIZATIONS,
};
use std::{
    ffi::{c_void, CString},
    fs::{File, Metadata},
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::{Path, PathBuf},
    ptr,
};

type Ref = *const c_void;
type Status = i32;
#[repr(C)]
struct Attribute {
    tag: u32,
    length: u32,
    data: *mut c_void,
}
#[repr(C)]
struct Attributes {
    count: u32,
    attributes: *mut Attribute,
}
#[repr(C)]
struct AttributeInfo {
    count: u32,
    tags: *mut u32,
    formats: *mut u32,
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(value: Ref);
    fn CFGetTypeID(value: Ref) -> usize;
    fn CFEqual(a: Ref, b: Ref) -> u8;
    fn CFArrayGetTypeID() -> usize;
    fn CFArrayCreate(
        allocator: Ref,
        values: *const Ref,
        count: isize,
        callbacks: *const c_void,
    ) -> Ref;
    static kCFTypeArrayCallBacks: [usize; 5];
    fn CFArrayGetCount(array: Ref) -> isize;
    fn CFArrayGetValueAtIndex(array: Ref, index: isize) -> Ref;
    fn CFURLCreateFromFileSystemRepresentation(
        allocator: Ref,
        bytes: *const u8,
        length: isize,
        directory: u8,
    ) -> Ref;
    fn CFStringCreateWithBytes(
        allocator: Ref,
        bytes: *const u8,
        length: isize,
        encoding: u32,
        external: u8,
    ) -> Ref;
    fn CFDictionaryGetValue(dictionary: Ref, key: Ref) -> Ref;
    fn CFNumberGetValue(number: Ref, kind: isize, value: *mut c_void) -> u8;
    fn CFNumberGetTypeID() -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFStringGetLength(string: Ref) -> isize;
    fn CFStringGetCString(string: Ref, buffer: *mut i8, size: isize, encoding: u32) -> u8;
}
#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    fn SecRequirementCreateWithString(text: Ref, flags: u32, requirement: *mut Ref) -> Status;
    fn SecStaticCodeCreateWithPath(path: Ref, flags: u32, code: *mut Ref) -> Status;
    fn SecStaticCodeCheckValidity(code: Ref, flags: u32, requirement: Ref) -> Status;
    fn SecCodeCopySigningInformation(code: Ref, flags: u32, info: *mut Ref) -> Status;
    fn SecCodeCopyDesignatedRequirement(code: Ref, flags: u32, requirement: *mut Ref) -> Status;
    static kSecCodeInfoFlags: Ref;
    static kSecCodeInfoTeamIdentifier: Ref;
    fn SecTrustedApplicationCreateFromPath(path: *const i8, app: *mut Ref) -> Status;
    fn SecTrustedApplicationValidateWithPath(app: Ref, path: *const i8) -> Status;
    fn SecTrustedApplicationGetTypeID() -> usize;
    fn SecAccessCreate(description: Ref, applications: Ref, access: *mut Ref) -> Status;
    fn SecAccessCopyACLList(access: Ref, list: *mut Ref) -> Status;
    fn SecACLCopyAuthorizations(acl: Ref) -> Ref;
    fn SecACLCopyContents(
        acl: Ref,
        apps: *mut Ref,
        description: *mut Ref,
        prompt: *mut u16,
    ) -> Status;
    static kSecACLAuthorizationAny: Ref;
    static kSecACLAuthorizationDecrypt: Ref;
    static kSecACLAuthorizationChangeACL: Ref;
    static kSecACLAuthorizationChangeOwner: Ref;
    static kSecACLAuthorizationEncrypt: Ref;
    static kSecACLAuthorizationSign: Ref;
    static kSecACLAuthorizationMAC: Ref;
    static kSecACLAuthorizationDerive: Ref;
    static kSecACLAuthorizationExportClear: Ref;
    static kSecACLAuthorizationExportWrapped: Ref;
    static kSecACLAuthorizationIntegrity: Ref;
    static kSecACLAuthorizationPartitionID: Ref;
    fn SecKeychainCopyDomainDefault(domain: i32, keychain: *mut Ref) -> Status;
    fn SecKeychainSearchCreateFromAttributes(
        keychain: Ref,
        class: u32,
        attrs: *const Attributes,
        search: *mut Ref,
    ) -> Status;
    fn SecKeychainSearchCopyNext(search: Ref, item: *mut Ref) -> Status;
    fn SecKeychainItemCreateFromContent(
        class: u32,
        attrs: *const Attributes,
        length: u32,
        data: *const c_void,
        keychain: Ref,
        access: Ref,
        item: *mut Ref,
    ) -> Status;
    fn SecKeychainItemModifyAttributesAndData(
        item: Ref,
        attrs: *const Attributes,
        length: u32,
        data: *const c_void,
    ) -> Status;
    fn SecKeychainItemCopyAccess(item: Ref, access: *mut Ref) -> Status;
    fn SecKeychainItemCopyAttributesAndData(
        item: Ref,
        info: *const AttributeInfo,
        class: *mut u32,
        attrs: *mut *mut Attributes,
        length: *mut u32,
        data: *mut *mut c_void,
    ) -> Status;
    fn SecKeychainItemFreeAttributesAndData(attrs: *mut Attributes, data: *mut c_void) -> Status;
}

// Every native +1 CF/Security reference is owned immediately, including errors.
// No unsafe Send/Sync: a capture/item stays on its creating worker.
struct Owned(Ref);
impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: all Owned values originate from a documented +1 result.
            unsafe { CFRelease(self.0) };
        }
    }
}
impl Owned {
    fn new(value: Ref) -> Result<Self, Error> {
        if value.is_null() {
            Err(Error::Native)
        } else {
            Ok(Self(value))
        }
    }
}
fn status(code: Status) -> Result<(), Error> {
    match code {
        0 => Ok(()),
        -25300 => Err(Error::Missing),
        -25299 => Err(Error::Collision),
        -25308 | -25293 | -25291 => Err(Error::Unavailable),
        _ => Err(Error::Native),
    }
}
fn returned(call: impl FnOnce(*mut Ref) -> Status) -> Result<Owned, Error> {
    let mut raw = ptr::null();
    let result = call(&mut raw);
    let owned = Owned(raw);
    status(result)?;
    if owned.0.is_null() {
        return Err(Error::Native);
    }
    Ok(owned)
}
fn array(value: Ref, max: usize) -> Result<Vec<Ref>, Error> {
    // SAFETY: callers supply live CF objects; type is checked before array access.
    unsafe {
        if value.is_null() || CFGetTypeID(value) != CFArrayGetTypeID() {
            return Err(Error::Policy);
        }
        let count = CFArrayGetCount(value);
        if count < 0 || count as usize > max {
            return Err(Error::Policy);
        }
        Ok((0..count)
            .map(|i| CFArrayGetValueAtIndex(value, i))
            .collect())
    }
}
fn equal(a: Ref, b: Ref) -> bool {
    !a.is_null() && !b.is_null() && unsafe { CFEqual(a, b) != 0 }
}
fn text(bytes: &[u8]) -> Result<Owned, Error> {
    // SAFETY: borrowed bytes have their stated bounded length; CF copies them.
    Owned::new(unsafe {
        CFStringCreateWithBytes(
            ptr::null(),
            bytes.as_ptr(),
            bytes.len() as isize,
            0x08000100,
            0,
        )
    })
}

#[derive(Eq, PartialEq)]
struct Identity {
    device: u64,
    inode: u64,
    length: u64,
    modified: (i64, i64),
}
fn identity(meta: &Metadata) -> Identity {
    Identity {
        device: meta.dev(),
        inode: meta.ino(),
        length: meta.len(),
        modified: (meta.mtime(), meta.mtime_nsec()),
    }
}
fn file(path: &Path, held: &File) -> Result<Identity, Error> {
    let disk = std::fs::symlink_metadata(path).map_err(|_| Error::InvalidInput)?;
    let open = held.metadata().map_err(|_| Error::InvalidInput)?;
    let uid = super::super::current_user_id().map_err(|_| Error::InvalidInput)?;
    if !path.is_absolute()
        || !disk.is_file()
        || disk.mode() & 0o022 != 0
        || ![0, uid].contains(&disk.uid())
        || disk.mode() & 0o111 == 0
        || identity(&disk) != identity(&open)
    {
        return Err(Error::InvalidInput);
    }
    Ok(identity(&open))
}
struct Application {
    app: Owned,
    code: Owned,
    _requirement: Owned,
    path: PathBuf,
    cpath: CString,
    held: File,
    identity: Identity,
    team: String,
}
impl Application {
    fn capture(path: PathBuf, held: File, helper: bool) -> Result<Self, Error> {
        let before = file(&path, &held)?;
        let bytes = path.as_os_str().as_bytes();
        let cpath = CString::new(bytes).map_err(|_| Error::InvalidInput)?;
        // SAFETY: valid borrowed path bytes; returned URL/code are retained.
        let url = Owned::new(unsafe {
            CFURLCreateFromFileSystemRepresentation(
                ptr::null(),
                bytes.as_ptr(),
                bytes.len() as isize,
                0,
            )
        })?;
        let code = returned(|out| unsafe { SecStaticCodeCreateWithPath(url.0, 0, out) })?;
        status(unsafe { SecStaticCodeCheckValidity(code.0, 1 << 4, ptr::null()) })
            .map_err(|_| Error::UnsupportedIdentity)?;
        let info = returned(|out| unsafe { SecCodeCopySigningInformation(code.0, 1 << 1, out) })?;
        // A trusted-app ACL alone does not cross the creator's legacy partition.
        let anchor = text(b"anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists and certificate leaf[field.1.2.840.113635.100.6.1.13] exists")?;
        let developer_id =
            returned(|out| unsafe { SecRequirementCreateWithString(anchor.0, 0, out) })?;
        let developer_id_valid =
            unsafe { SecStaticCodeCheckValidity(code.0, 1 << 4, developer_id.0) } == 0;
        let (bits, team) = unsafe {
            let flags = CFDictionaryGetValue(info.0, kSecCodeInfoFlags);
            let team = CFDictionaryGetValue(info.0, kSecCodeInfoTeamIdentifier);
            let mut bits = 0_i32;
            if flags.is_null()
                || CFGetTypeID(flags) != CFNumberGetTypeID()
                || CFNumberGetValue(flags, 3, (&mut bits as *mut i32).cast()) == 0
                || team.is_null()
                || CFGetTypeID(team) != CFStringGetTypeID()
                || !(1..=128).contains(&CFStringGetLength(team))
            {
                return Err(Error::UnsupportedIdentity);
            }
            let mut bytes = [0_i8; 513];
            if CFStringGetCString(team, bytes.as_mut_ptr(), bytes.len() as isize, 0x08000100) == 0 {
                return Err(Error::UnsupportedIdentity);
            }
            let team = std::ffi::CStr::from_ptr(bytes.as_ptr())
                .to_str()
                .map_err(|_| Error::UnsupportedIdentity)?
                .to_owned();
            (bits, team)
        };
        compatible_identities(
            developer_id_valid,
            bits & 2 != 0,
            Some(&team),
            developer_id_valid,
            bits & 2 != 0,
            Some(&team),
        )?;
        let requirement =
            returned(|out| unsafe { SecCodeCopyDesignatedRequirement(code.0, 0, out) })
                .map_err(|_| Error::UnsupportedIdentity)?;
        // NULL captures the actual executing application's designated identity,
        // rather than trusting whatever currently occupies current_exe's path.
        // Revalidation below must match that identity against retained disk code.
        let app = returned(|out| unsafe {
            SecTrustedApplicationCreateFromPath(
                if helper { cpath.as_ptr() } else { ptr::null() },
                out,
            )
        })?;
        let result = Self {
            app,
            code,
            _requirement: requirement,
            path,
            cpath,
            held,
            identity: before,
            team,
        };
        result.revalidate()?;
        Ok(result)
    }
    fn revalidate(&self) -> Result<(), Error> {
        if file(&self.path, &self.held)? != self.identity {
            return Err(Error::UnsupportedIdentity);
        }
        status(unsafe { SecStaticCodeCheckValidity(self.code.0, 1 << 4, ptr::null()) })
            .map_err(|_| Error::UnsupportedIdentity)?;
        status(unsafe { SecTrustedApplicationValidateWithPath(self.app.0, self.cpath.as_ptr()) })
            .map_err(|_| Error::UnsupportedIdentity)
    }
}

/// In-memory access for this exact executing GUI and one authenticated helper.
/// Caller must first authenticate the helper's immutable stage and consent.
/// No publisher authority is inferred from a path or this object.
pub struct Applications {
    gui: Application,
    helper: Application,
    access: Owned,
}
impl Applications {
    /// Captures the executing Developer ID app and same-Team authenticated HOST,
    /// never an arbitrary GUI. Ad-hoc identities cannot share the default partition.
    pub fn capture(
        authenticated_helper_path: &Path,
        authenticated_helper: &File,
    ) -> Result<Self, Error> {
        let path = std::env::current_exe().map_err(|_| Error::UnsupportedIdentity)?;
        let held = File::open(&path).map_err(|_| Error::UnsupportedIdentity)?;
        let gui = Application::capture(path, held, false)?;
        let helper = Application::capture(
            authenticated_helper_path.to_owned(),
            authenticated_helper
                .try_clone()
                .map_err(|_| Error::InvalidInput)?,
            true,
        )?;
        compatible_identities(
            true,
            false,
            Some(&gui.team),
            true,
            false,
            Some(&helper.team),
        )?;
        // Distinct identities, not two aliases for the same application.
        if unsafe { SecTrustedApplicationValidateWithPath(gui.app.0, helper.cpath.as_ptr()) } == 0
            || unsafe { SecTrustedApplicationValidateWithPath(helper.app.0, gui.cpath.as_ptr()) }
                == 0
        {
            return Err(Error::Policy);
        }
        let apps = [gui.app.0, helper.app.0];
        let list = Owned::new(unsafe {
            CFArrayCreate(
                ptr::null(),
                apps.as_ptr(),
                2,
                ptr::addr_of!(kCFTypeArrayCallBacks).cast(),
            )
        })?;
        let description = text(b"fesTerm HOST installation")?;
        let access = returned(|out| unsafe { SecAccessCreate(description.0, list.0, out) })?;
        let result = Self {
            gui,
            helper,
            access,
        };
        result.revalidate()?;
        result.verify_access(result.access.0)?;
        Ok(result)
    }
    fn revalidate(&self) -> Result<(), Error> {
        self.gui.revalidate()?;
        self.helper.revalidate()
    }
    fn verify_access(&self, access: Ref) -> Result<(), Error> {
        let list = returned(|out| unsafe { SecAccessCopyACLList(access, out) })?;
        let mut entries = Vec::new();
        for acl in array(list.0, MAX_ACLS)? {
            let authorizations = Owned::new(unsafe { SecACLCopyAuthorizations(acl) })?;
            let mut decrypt = false;
            let mut administration = false;
            for auth in array(authorizations.0, MAX_AUTHORIZATIONS)? {
                // Every known standard tag is finite. Unknown forms refuse.
                let known = unsafe {
                    [
                        kSecACLAuthorizationAny,
                        kSecACLAuthorizationDecrypt,
                        kSecACLAuthorizationChangeACL,
                        kSecACLAuthorizationChangeOwner,
                        kSecACLAuthorizationEncrypt,
                        kSecACLAuthorizationSign,
                        kSecACLAuthorizationMAC,
                        kSecACLAuthorizationDerive,
                        kSecACLAuthorizationExportClear,
                        kSecACLAuthorizationExportWrapped,
                        kSecACLAuthorizationIntegrity,
                        kSecACLAuthorizationPartitionID,
                    ]
                };
                if !known.iter().any(|tag| equal(auth, *tag)) {
                    return Err(Error::Policy);
                }
                unsafe {
                    let any = equal(auth, kSecACLAuthorizationAny);
                    decrypt |= any || equal(auth, kSecACLAuthorizationDecrypt);
                    administration |= any
                        || equal(auth, kSecACLAuthorizationChangeACL)
                        || equal(auth, kSecACLAuthorizationChangeOwner);
                }
            }
            if !decrypt && !administration {
                entries.push((false, false, None));
                continue;
            }
            let mut apps = ptr::null();
            let mut description = ptr::null();
            let mut selector = 0_u16; // public SecKeychainPromptSelector ABI
            let result =
                unsafe { SecACLCopyContents(acl, &mut apps, &mut description, &mut selector) };
            let apps = Owned(apps);
            let _description = Owned(description);
            status(result)?;
            let roles = if apps.0.is_null() {
                None
            } else {
                let mut roles = Vec::new();
                for app in array(apps.0, 2)? {
                    if app.is_null()
                        || unsafe { CFGetTypeID(app) != SecTrustedApplicationGetTypeID() }
                    {
                        return Err(Error::Policy);
                    }
                    let gui = unsafe {
                        SecTrustedApplicationValidateWithPath(app, self.gui.cpath.as_ptr())
                    } == 0;
                    let helper = unsafe {
                        SecTrustedApplicationValidateWithPath(app, self.helper.cpath.as_ptr())
                    } == 0;
                    roles.push(match (gui, helper) {
                        (true, false) => 0,
                        (false, true) => 1,
                        _ => return Err(Error::Policy),
                    });
                }
                Some(roles)
            };
            entries.push((decrypt, administration, roles));
        }
        policy(&entries)
    }
    /// Create only. API verification failure leaves the reserved item intact
    /// for its caller's interrupted-publication evidence; never deletes/repairs.
    pub fn create_new(&self, service: &[u8], account: &[u8], secret: &[u8]) -> Result<Item, Error> {
        input(service, account, secret)?;
        self.revalidate()?;
        let keychain = keychain()?;
        let mut attrs = attributes(service, account);
        let list = Attributes {
            count: 2,
            attributes: attrs.as_mut_ptr(),
        };
        let item = create_if_missing(
            || lookup(keychain.0, service, account).map(|_| ()),
            || {
                returned(|out| unsafe {
                    SecKeychainItemCreateFromContent(
                        GENERIC,
                        &list,
                        secret.len() as u32,
                        secret.as_ptr().cast(),
                        keychain.0,
                        self.access.0,
                        out,
                    )
                })
            },
        )?;
        let result = Item {
            item,
            keychain,
            service: service.to_vec(),
            account: account.to_vec(),
        };
        self.revalidate()?;
        let actual = returned(|out| unsafe { SecKeychainItemCopyAccess(result.item.0, out) })?;
        self.verify_access(actual.0)?;
        Ok(result)
    }
}
const GENERIC: u32 = u32::from_be_bytes(*b"genp");
fn keychain() -> Result<Owned, Error> {
    returned(|out| unsafe { SecKeychainCopyDomainDefault(0, out) })
}
fn attributes(service: &[u8], account: &[u8]) -> [Attribute; 2] {
    [
        Attribute {
            tag: u32::from_be_bytes(*b"svce"),
            length: service.len() as u32,
            data: service.as_ptr().cast_mut().cast(),
        },
        Attribute {
            tag: u32::from_be_bytes(*b"acct"),
            length: account.len() as u32,
            data: account.as_ptr().cast_mut().cast(),
        },
    ]
}
fn lookup(keychain: Ref, service: &[u8], account: &[u8]) -> Result<Owned, Error> {
    let mut attrs = attributes(service, account);
    let list = Attributes {
        count: 2,
        attributes: attrs.as_mut_ptr(),
    };
    let search = returned(|out| unsafe {
        SecKeychainSearchCreateFromAttributes(keychain, GENERIC, &list, out)
    })?;
    let item = returned(|out| unsafe { SecKeychainSearchCopyNext(search.0, out) })?;
    match returned(|out| unsafe { SecKeychainSearchCopyNext(search.0, out) }) {
        Err(Error::Missing) => {
            verify_selected(item.0, service, account)?;
            Ok(item)
        }
        Ok(_) => Err(Error::Policy),
        Err(e) => Err(e),
    }
}
/// Retained selected logical item. No Debug, secret read, delete or ACL mutation.
pub struct Item {
    item: Owned,
    keychain: Owned,
    service: Vec<u8>,
    account: Vec<u8>,
}
impl Item {
    /// Exact selected metadata lookup, with no secret read or ACL rewriting.
    pub fn open_existing(service: &[u8], account: &[u8]) -> Result<Self, Error> {
        input(service, account, &[])?;
        let keychain = keychain()?;
        let item = lookup(keychain.0, service, account)?;
        Ok(Self {
            item,
            keychain,
            service: service.to_vec(),
            account: account.to_vec(),
        })
    }
    /// Update through the retained item; no create fallback on any failure.
    /// This preserves supported-path identity, not hostile same-user isolation.
    pub fn update(&self, secret: &[u8]) -> Result<(), Error> {
        input(&self.service, &self.account, secret)?;
        update_selected(
            || {
                let current = lookup(self.keychain.0, &self.service, &self.account)?;
                if !equal(current.0, self.item.0) {
                    return Err(Error::Policy);
                }
                Ok(())
            },
            || {
                status(unsafe {
                    SecKeychainItemModifyAttributesAndData(
                        self.item.0,
                        ptr::null(),
                        secret.len() as u32,
                        secret.as_ptr().cast(),
                    )
                })
            },
        )
    }
}

// Attribute-only verification never requests/decrypts a secret. Native memory
// is released even when the API reports an error or an unexpected shape.
fn verify_selected(item: Ref, service: &[u8], account: &[u8]) -> Result<(), Error> {
    struct AttributeMemory(*mut Attributes);
    impl Drop for AttributeMemory {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    SecKeychainItemFreeAttributesAndData(self.0, ptr::null_mut());
                }
            }
        }
    }
    let mut tags = [u32::from_be_bytes(*b"svce"), u32::from_be_bytes(*b"acct")];
    let mut formats = [0_u32; 2]; // CSSM_DB_ATTRIBUTE_FORMAT_STRING
    let info = AttributeInfo {
        count: 2,
        tags: tags.as_mut_ptr(),
        formats: formats.as_mut_ptr(),
    };
    let mut raw = ptr::null_mut();
    let mut class = 0_u32;
    let result = unsafe {
        SecKeychainItemCopyAttributesAndData(
            item,
            &info,
            &mut class,
            &mut raw,
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };
    let memory = AttributeMemory(raw);
    status(result)?;
    if memory.0.is_null() || class != GENERIC {
        return Err(Error::Policy);
    }
    // SAFETY: successful API result owns this list; validate length/pointers
    // before making bounded slices. Only the requested two attributes are used.
    unsafe {
        let list = &*memory.0;
        if list.count != 2 || list.attributes.is_null() {
            return Err(Error::Policy);
        }
        let mut seen = [false; 2];
        for attribute in std::slice::from_raw_parts(list.attributes, 2) {
            let (index, expected) = match attribute.tag {
                tag if tag == tags[0] => (0, service),
                tag if tag == tags[1] => (1, account),
                _ => return Err(Error::Policy),
            };
            if seen[index]
                || attribute.length as usize != expected.len()
                || attribute.data.is_null()
            {
                return Err(Error::Policy);
            }
            if std::slice::from_raw_parts(attribute.data.cast::<u8>(), expected.len()) != expected {
                return Err(Error::Policy);
            }
            seen[index] = true;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRetain(value: Ref) -> Ref;
        fn CFGetRetainCount(value: Ref) -> isize;
    }
    #[test]
    fn mac_host_owned_partial_error_releases_retained_native_output() {
        // CoreFoundation-only allocation: never opens a Keychain, code object,
        // item, user interface or credential store on the host running tests.
        let original = text(
            b"owned native output fixture long enough for a heap allocation, no secret material",
        )
        .unwrap();
        let count = unsafe { CFGetRetainCount(original.0) };
        assert!(matches!(
            returned(|out| {
                unsafe {
                    *out = CFRetain(original.0);
                }
                -1
            }),
            Err(Error::Native)
        ));
        assert_eq!(unsafe { CFGetRetainCount(original.0) }, count);
        assert!(matches!(returned(|_| 0), Err(Error::Native)));
        let second = returned(|out| {
            unsafe {
                *out = CFRetain(original.0);
            }
            0
        })
        .unwrap();
        assert_eq!(unsafe { CFGetRetainCount(original.0) }, count + 1);
        drop(second);
        assert_eq!(unsafe { CFGetRetainCount(original.0) }, count);
    }
}
