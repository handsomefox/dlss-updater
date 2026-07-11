use dlss_core::{
    CoreError, DllInspector, DllMetadata, DllVersion, RevocationStatus, SignatureStatus,
    TrustPolicy, TrustReport, TrustVerifier,
};
use object::{Object, ObjectKind};
use sha2::{Digest, Sha256};
use std::{fs::File, io::Read, path::Path};

/// Portable PE inspection used in tests and future Proton support. Windows adds version resources.
pub struct PortablePeInspector;
impl DllInspector for PortablePeInspector {
    fn inspect(&self, path: &Path) -> Result<DllMetadata, CoreError> {
        let mut bytes = Vec::new();
        File::open(path)?.read_to_end(&mut bytes)?;
        let object =
            object::File::parse(&*bytes).map_err(|e| CoreError::Validation(e.to_string()))?;
        let x86_64 = object.format() == object::BinaryFormat::Pe
            && object.architecture() == object::Architecture::X86_64
            && object.kind() == ObjectKind::Dynamic;
        Ok(DllMetadata {
            version: portable_file_version(&object),
            sha256: Sha256::digest(&bytes).into(),
            signature: SignatureStatus::Unavailable,
            x86_64,
        })
    }
}

// PE version-resource parsing belongs to the Windows adapter. Keeping Unknown is safer than timestamps/strings.
fn portable_file_version(_object: &object::File<'_>) -> Option<DllVersion> {
    None
}

pub struct UnavailableTrustVerifier;
impl TrustVerifier for UnavailableTrustVerifier {
    fn verify(&self, _path: &Path, _policy: TrustPolicy) -> Result<TrustReport, CoreError> {
        Ok(TrustReport {
            signature: SignatureStatus::Unavailable,
            signer: None,
            revocation: RevocationStatus::NotVerified,
            native_failure: None,
        })
    }
}
