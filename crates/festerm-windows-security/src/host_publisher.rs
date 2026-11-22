//! Compiled ADR0084 publisher policy. Synthetic decisions are not native evidence.
use std::io;

const SUBJECT_FIELDS: [(&str, &str); 5] = [
    ("2.5.4.6", "US"),
    ("2.5.4.8", "wa"),
    ("2.5.4.7", "Seattle"),
    ("2.5.4.10", "Frank Swiderski"),
    ("2.5.4.3", "Frank Swiderski"),
];
const PROFILE: &str = "1.3.6.1.4.1.311.97.303022872.891206086.435023656.127014475";
const CODE_SIGNING: &str = "1.3.6.1.5.5.7.3.3";
const PUBLIC_TRUST: &str = "1.3.6.1.4.1.311.97.1.0";

// SHA256 of DER certificates obtained from Microsoft's public PKI repository,
// never from a candidate, manifest, account name or installed application.
const APPROVED_CHAIN: [[u8; 32]; 3] = [
    [
        0xac, 0xaa, 0x07, 0xd5, 0x7d, 0x42, 0x74, 0xb2, 0x90, 0xd8, 0x6c, 0x15, 0x6a, 0xa0, 0xd6,
        0xc5, 0x63, 0x3e, 0x3c, 0xd9, 0x2d, 0x43, 0x75, 0x1e, 0x6a, 0x0f, 0x79, 0x7c, 0x5b, 0x2d,
        0xff, 0x4d,
    ],
    [
        0x3d, 0x29, 0x79, 0x8c, 0xc5, 0xd3, 0xf0, 0x64, 0x4a, 0x7e, 0x0d, 0xc9, 0xcb, 0x1c, 0xad,
        0xe5, 0x23, 0xea, 0x5e, 0xc8, 0x3b, 0x33, 0x51, 0x09, 0xb6, 0x05, 0xbf, 0xea, 0xa7, 0xd5,
        0xf5, 0xc1,
    ],
    [
        0x53, 0x67, 0xf2, 0x0c, 0x7a, 0xde, 0x0e, 0x2b, 0xca, 0x79, 0x09, 0x15, 0x05, 0x6d, 0x08,
        0x6b, 0x72, 0x0c, 0x33, 0xc1, 0xfa, 0x2a, 0x26, 0x61, 0xac, 0xf7, 0x87, 0xe3, 0x29, 0x2e,
        0x12, 0x70,
    ],
];

// Independently retrieved Microsoft ID Verified CS EOC CA 03. This is a
// second exact chain, not permission to accept other issuers under the root.
const APPROVED_EOC_CHAIN: [[u8; 32]; 3] = [
    [
        0xbf, 0xf5, 0xc1, 0xa5, 0x4b, 0x42, 0x1e, 0x0c, 0xc1, 0x23, 0x72, 0xf2, 0x21, 0xfa, 0x56,
        0xe6, 0xf1, 0xb0, 0x30, 0x5a, 0x9c, 0x07, 0x1a, 0x2a, 0xf1, 0xbf, 0x45, 0x89, 0x3d, 0x2a,
        0x46, 0x23,
    ],
    APPROVED_CHAIN[1],
    APPROVED_CHAIN[2],
];
// Independently retrieved Microsoft ID Verified CS EOC CA 04. The existing
// publisher profile, pinned PCA/root and native proofs remain mandatory.
const APPROVED_EOC04_CHAIN: [[u8; 32]; 3] = [
    [
        0xbc, 0x30, 0x95, 0x55, 0xfa, 0x42, 0xc5, 0x63, 0xb8, 0xee, 0xbf, 0xb7, 0x11, 0x76, 0x95,
        0xc6, 0xba, 0xe8, 0x9e, 0x4b, 0xa1, 0x9c, 0x03, 0xcb, 0x72, 0x19, 0x79, 0xa5, 0x63, 0xa6,
        0x09, 0x29,
    ],
    APPROVED_CHAIN[1],
    APPROVED_CHAIN[2],
];
// Independently retrieved Microsoft ID Verified CS AOC CA 04. This is distinct
// from EOC CA 04; all existing exact chains and native proofs remain required.
const APPROVED_AOC04_CHAIN: [[u8; 32]; 3] = [
    [
        0x15, 0xce, 0xf5, 0xf6, 0x3c, 0xa6, 0xd1, 0xf0, 0x22, 0xb2, 0x93, 0xe9, 0x2c, 0x61, 0xc0,
        0x05, 0x9c, 0x49, 0xbb, 0x92, 0xee, 0xcd, 0xbc, 0xde, 0x3a, 0x12, 0x5d, 0x3c, 0x63, 0x56,
        0xd9, 0x4f,
    ],
    APPROVED_CHAIN[1],
    APPROVED_CHAIN[2],
];
const APPROVED_CHAINS: [[[u8; 32]; 3]; 4] = [
    APPROVED_CHAIN,
    APPROVED_EOC_CHAIN,
    APPROVED_EOC04_CHAIN,
    APPROVED_AOC04_CHAIN,
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Revocation {
    FreshGood,
    Revoked,
    Unknown,
    Stale,
}

struct Evidence<'a> {
    subject_matches: bool,
    ekus: &'a [String],
    chain: &'a [[u8; 32]],
    native_signature_and_digest: bool,
    trusted_rfc3161: bool,
    revocation: Revocation,
}

fn refused() -> io::Error {
    io::Error::other("host.service_windows_release_publisher_required")
}

fn approved_subject(fields: &[(String, String)]) -> bool {
    fields.len() == SUBJECT_FIELDS.len()
        && fields
            .iter()
            .zip(SUBJECT_FIELDS)
            .all(|((oid, value), expected)| oid == expected.0 && value == expected.1)
}

fn admit(evidence: &Evidence<'_>) -> io::Result<()> {
    if !evidence.subject_matches
        || ![PROFILE, CODE_SIGNING, PUBLIC_TRUST]
            .iter()
            .all(|required| {
                evidence
                    .ekus
                    .iter()
                    .filter(|eku| eku.as_str() == *required)
                    .count()
                    == 1
            })
        || !APPROVED_CHAINS
            .iter()
            .any(|approved| evidence.chain == approved)
        || !evidence.native_signature_and_digest
        || !evidence.trusted_rfc3161
        || evidence.revocation != Revocation::FreshGood
    {
        return Err(refused());
    }
    Ok(())
}

#[cfg(windows)]
mod native;
#[cfg(windows)]
pub use native::verify;

#[cfg(test)]
mod tests {
    use super::*;

    fn verified_evidence<'a>(ekus: &'a [String], chain: &'a [[u8; 32]]) -> Evidence<'a> {
        Evidence {
            subject_matches: true,
            ekus,
            chain,
            native_signature_and_digest: true,
            trusted_rfc3161: true,
            revocation: Revocation::FreshGood,
        }
    }

    #[test]
    fn windows_publisher_policy_accepts_independently_pinned_aoc04_and_retains_prior_chains() {
        let ekus = [PROFILE, CODE_SIGNING, PUBLIC_TRUST].map(str::to_owned);
        // Canonical DER from the independently retrieved Microsoft AOC CA04,
        // distinct from the retained EOC CA04 certificate.
        let aoc04 = [
            [
                0x15, 0xce, 0xf5, 0xf6, 0x3c, 0xa6, 0xd1, 0xf0, 0x22, 0xb2, 0x93, 0xe9, 0x2c, 0x61,
                0xc0, 0x05, 0x9c, 0x49, 0xbb, 0x92, 0xee, 0xcd, 0xbc, 0xde, 0x3a, 0x12, 0x5d, 0x3c,
                0x63, 0x56, 0xd9, 0x4f,
            ],
            APPROVED_CHAIN[1],
            APPROVED_CHAIN[2],
        ];
        admit(&verified_evidence(&ekus, &aoc04)).unwrap();
        for previous in [APPROVED_CHAIN, APPROVED_EOC_CHAIN, APPROVED_EOC04_CHAIN] {
            admit(&verified_evidence(&ekus, &previous)).unwrap();
            assert_ne!(previous[0], aoc04[0]);
        }
        let mut unknown = aoc04;
        unknown[0][0] ^= 1;
        assert!(admit(&verified_evidence(&ekus, &unknown)).is_err());
        let short = &aoc04[..2];
        assert!(admit(&verified_evidence(&ekus, short)).is_err());
        let reordered = [aoc04[1], aoc04[0], aoc04[2]];
        assert!(admit(&verified_evidence(&ekus, &reordered)).is_err());
        let extra = [aoc04[0], APPROVED_EOC04_CHAIN[0], aoc04[1], aoc04[2]];
        assert!(admit(&verified_evidence(&ekus, &extra)).is_err());
        // Common PCA/root bytes form the same valid chain, not a mixed-chain refusal.
        let shared_ancestors = [aoc04[0], APPROVED_EOC04_CHAIN[1], APPROVED_EOC04_CHAIN[2]];
        admit(&verified_evidence(&ekus, &shared_ancestors)).unwrap();
    }

    #[test]
    fn windows_publisher_policy_accepts_only_approved_profile_and_fresh_native_proofs() {
        let ekus = [PROFILE, CODE_SIGNING, PUBLIC_TRUST].map(str::to_owned);
        for chain in &APPROVED_CHAINS {
            let mut evidence = verified_evidence(&ekus, chain);
            admit(&evidence).unwrap();
            for revocation in [Revocation::Revoked, Revocation::Unknown, Revocation::Stale] {
                evidence.revocation = revocation;
                assert!(admit(&evidence).is_err());
            }
            evidence.revocation = Revocation::FreshGood;
            evidence.subject_matches = false;
            assert!(admit(&evidence).is_err());
            evidence.subject_matches = true;
            evidence.native_signature_and_digest = false;
            assert!(admit(&evidence).is_err());
            evidence.native_signature_and_digest = true;
            evidence.trusted_rfc3161 = false;
            assert!(admit(&evidence).is_err());
        }
    }

    #[test]
    fn windows_publisher_policy_rejects_missing_wrong_and_duplicate_ekus() {
        for chain in &APPROVED_CHAINS {
            for index in 0..3 {
                let mut ekus = [PROFILE, CODE_SIGNING, PUBLIC_TRUST]
                    .map(str::to_owned)
                    .to_vec();
                ekus[index] = "1.2.3.4".into();
                let mut evidence = verified_evidence(&ekus, chain);
                assert!(admit(&evidence).is_err());
                evidence.ekus = &[];
                assert!(admit(&evidence).is_err());
                let duplicates = [PROFILE, CODE_SIGNING, PUBLIC_TRUST, PROFILE].map(str::to_owned);
                evidence.ekus = &duplicates;
                assert!(admit(&evidence).is_err());
            }
        }
    }

    #[test]
    fn windows_publisher_policy_pins_full_chain_not_candidate_ca_names() {
        let ekus = [PROFILE, CODE_SIGNING, PUBLIC_TRUST].map(str::to_owned);
        for approved in APPROVED_CHAINS {
            for certificate in 0..3 {
                for byte in 0..32 {
                    let mut mutated = approved;
                    mutated[certificate][byte] ^= 1;
                    assert!(admit(&verified_evidence(&ekus, &mutated)).is_err());
                }
            }
        }
    }

    #[test]
    fn windows_publisher_policy_rejects_mixed_reordered_and_incomplete_chains() {
        let ekus = [PROFILE, CODE_SIGNING, PUBLIC_TRUST].map(str::to_owned);
        for approved in &APPROVED_CHAINS {
            for other in &APPROVED_CHAINS {
                if other == approved {
                    continue;
                }
                // Each certificate is approved somewhere; membership in a bag
                // of pins does not authorize a mixed or extended chain.
                for position in 1..3 {
                    let mut mixed = *approved;
                    mixed[position] = other[0];
                    assert!(admit(&verified_evidence(&ekus, &mixed)).is_err());
                }
                let extended = [approved[0], other[0], approved[1], approved[2]];
                assert!(admit(&verified_evidence(&ekus, &extended)).is_err());
            }
            for order in [[0, 2, 1], [1, 0, 2], [1, 2, 0], [2, 0, 1], [2, 1, 0]] {
                let reordered = order.map(|position| approved[position]);
                assert!(admit(&verified_evidence(&ekus, &reordered)).is_err());
            }
            for length in 0..3 {
                assert!(admit(&verified_evidence(&ekus, &approved[..length])).is_err());
            }
        }
    }

    #[test]
    fn windows_publisher_subject_requires_exact_rdns_without_extra_or_duplicate_attributes() {
        let expected = SUBJECT_FIELDS.map(|(oid, value)| (oid.to_owned(), value.to_owned()));
        assert!(approved_subject(&expected));
        for index in 0..expected.len() {
            let mut fields = expected.clone();
            fields[index].1.push_str(" foreign");
            assert!(!approved_subject(&fields));
        }
        assert!(!approved_subject(&expected[..4]));
        let mut extra = expected.to_vec();
        extra.push(expected[0].clone());
        assert!(!approved_subject(&extra));
        let mut reordered = expected.clone();
        reordered.swap(0, 1);
        assert!(!approved_subject(&reordered));
        let mut nul = expected.clone();
        nul[4].1.push('\0');
        assert!(!approved_subject(&nul));
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "read-only native PKI fixture requires owner-supplied approved signed host and online revocation; not run by cross-checks"]
    fn windows_publisher_native_opt_in_approved_signed_host_and_unsigned_rejection() {
        let path = std::path::PathBuf::from(
            std::env::var_os("FESTERM_ADR84_SIGNED_HOST_FIXTURE")
                .expect("set public signed host fixture pathname explicitly"),
        );
        assert!(path.is_absolute());
        let file = std::fs::File::open(&path).unwrap();
        verify(&file, &path).expect("approved native publisher, RFC3161 and fresh revocation");
        let unsigned = std::env::current_exe().unwrap();
        let file = std::fs::File::open(&unsigned).unwrap();
        assert!(verify(&file, &unsigned).is_err());
        // This is PKI-only evidence, never task/runtime/vault/cloud qualification.
        eprintln!("native_task_runtime_qualification=false; fixture=read-only-publisher");
    }
}
