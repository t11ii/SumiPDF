//! Per-user preferences. Each setting owns one independently replaced file.
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    ffi::OsString,
    fs,
    io::{ErrorKind, Read, Write},
    path::{Path, PathBuf},
};

const MAX_RECORD_BYTES: usize = 1024 * 1024;
const WATERMARK_FILE: &str = "watermark.json";
const SIGNATURE_IMAGE_FILE: &str = "signatureImage.json";
const IDENTITY_FILE: &str = "identity.json";
const RESTORE_TABS_FILE: &str = "restoreTabs.json";
const TAB_SESSION_FILE: &str = "session.json";

#[derive(Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct TabSession {
    pub active: Option<OsString>,
    pub tabs: Vec<RememberedTab>,
}

#[derive(Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct RememberedTab {
    pub path: OsString,
    pub page: u32,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub(crate) enum Identity {
    Pfx(PfxIdentity),
    Store { name: String, thumbprint: [u8; 20] },
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PfxIdentity {
    pub path: PathBuf,
    pub name: String,
    pub thumbprint: [u8; 20],
    pub file_hash: [u8; 32],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protected_password: Option<Vec<u8>>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Watermark {
    path: Option<PathBuf>,
}

struct Preferences(PathBuf);

impl Preferences {
    fn from_profile(local_app_data: Option<OsString>) -> Result<Self, String> {
        let directory = local_app_data
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or("The Windows local application data folder is unavailable.")?;
        Ok(Self(directory.join("SumiPDF")))
    }

    fn read<T: DeserializeOwned>(&self, filename: &str) -> Result<Option<T>, String> {
        let file = match fs::File::open(self.0.join(filename)) {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("Could not open {filename}. {error}")),
        };
        let mut bytes = Vec::new();
        file.take(MAX_RECORD_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("Could not read {filename}. {e}"))?;
        if bytes.len() > MAX_RECORD_BYTES {
            return Err(format!("{filename} exceeds the settings size limit."));
        }
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| format!("Could not read the settings in {filename}. {e}"))
    }

    fn write<T: Serialize + ?Sized>(
        &self,
        filename: &str,
        record: Option<&T>,
    ) -> Result<(), String> {
        let destination = self.0.join(filename);
        let Some(record) = record else {
            return match fs::remove_file(destination) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
                Err(e) => Err(format!("Could not forget {filename}. {e}")),
            };
        };
        let data =
            serde_json::to_vec(record).map_err(|e| format!("Could not save {filename}. {e}"))?;
        if data.len() > MAX_RECORD_BYTES {
            return Err(format!("{filename} exceeds the settings size limit."));
        }
        // The directory and its files inherit the user's LocalAppData permissions on Windows.
        fs::create_dir_all(&self.0)
            .map_err(|e| format!("Could not create the settings folder. {e}"))?;
        let mut file = tempfile::NamedTempFile::new_in(&self.0)
            .map_err(|e| format!("Could not create a temporary settings file. {e}"))?;
        file.write_all(&data)
            .and_then(|()| file.as_file().sync_all())
            .map_err(|e| format!("Could not write {filename}. {e}"))?;
        file.persist(destination)
            .map_err(|e| format!("Could not save {filename}. {}", e.error))?;
        Ok(())
    }

    fn load_watermark(
        &self,
        default: impl FnOnce() -> Option<PathBuf>,
    ) -> Result<Option<PathBuf>, String> {
        let Some(record) = self.read::<Watermark>(WATERMARK_FILE)? else {
            return Ok(default());
        };
        if let Some(path) = &record.path {
            validate_path(path)?;
        }
        Ok(record.path)
    }

    fn save_watermark(&self, path: Option<&Path>) -> Result<(), String> {
        if let Some(path) = path {
            validate_path(path)?;
        }
        self.write(
            WATERMARK_FILE,
            Some(&Watermark {
                path: path.map(Path::to_owned),
            }),
        )
    }

    fn load_signature_image(&self) -> Result<Option<PathBuf>, String> {
        let path: Option<PathBuf> = self.read(SIGNATURE_IMAGE_FILE)?;
        if let Some(path) = &path {
            validate_path(path)?;
        }
        Ok(path)
    }

    fn save_signature_image(&self, path: Option<&Path>) -> Result<(), String> {
        if let Some(path) = path {
            validate_path(path)?;
        }
        self.write(SIGNATURE_IMAGE_FILE, path)
    }

    fn load_identity(&self) -> Result<Option<Identity>, String> {
        let record = self.read::<Identity>(IDENTITY_FILE)?;
        if let Some(record) = &record {
            record.validate()?;
        }
        Ok(record)
    }

    fn save_identity(&self, record: Option<&Identity>) -> Result<(), String> {
        if let Some(record) = record {
            record.validate()?;
        }
        self.write(IDENTITY_FILE, record)
    }

    fn restore_tabs_enabled(&self) -> Result<bool, String> {
        Ok(self.read(RESTORE_TABS_FILE)?.unwrap_or(true))
    }

    fn set_restore_tabs_enabled(&self, enabled: bool) -> Result<(), String> {
        self.write(RESTORE_TABS_FILE, Some(&enabled))
    }

    fn load_tab_session(&self) -> Result<TabSession, String> {
        if !self.restore_tabs_enabled()? {
            return Ok(TabSession::default());
        }
        let mut session: TabSession = self.read(TAB_SESSION_FILE)?.unwrap_or_default();
        session.tabs.retain(|tab| {
            let path = Path::new(&tab.path);
            path.is_absolute() && !tab.path.as_encoded_bytes().contains(&0) && path.is_file()
        });
        Ok(session)
    }

    fn save_tab_session(&self, session: &TabSession) -> Result<(), String> {
        if self.restore_tabs_enabled()? {
            self.write(
                TAB_SESSION_FILE,
                (!session.tabs.is_empty()).then_some(session),
            )?;
        }
        Ok(())
    }
}

fn validate_path(path: &Path) -> Result<(), String> {
    if path.is_absolute() && path.to_str().is_some_and(|s| !s.contains('\0')) {
        Ok(())
    } else {
        Err("A remembered file needs a full Unicode path.".into())
    }
}

impl Identity {
    fn validate(&self) -> Result<(), String> {
        let name = match self {
            Self::Pfx(pfx) => {
                validate_path(&pfx.path)?;
                if pfx.protected_password.as_ref().is_some_and(Vec::is_empty) {
                    return Err("The protected PFX password record is empty.".into());
                }
                &pfx.name
            }
            Self::Store { name, .. } => name,
        };
        if name.is_empty() || name.contains('\0') {
            return Err("The remembered certificate name is invalid.".into());
        }
        Ok(())
    }
}

#[cfg(windows)]
fn for_user() -> Result<Preferences, String> {
    Preferences::from_profile(std::env::var_os("LOCALAPPDATA"))
}

#[cfg(windows)]
pub(crate) fn load_watermark() -> Result<Option<PathBuf>, String> {
    for_user()?.load_watermark(|| {
        let path = acrobat_security_folder()?.join("SignatureLogo.pdf");
        path.is_file().then_some(path)
    })
}

#[cfg(windows)]
pub(crate) fn acrobat_security_folder() -> Option<PathBuf> {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .map(|p| p.join(r"Adobe\Acrobat\DC\Security"))
}

#[cfg(windows)]
pub(crate) fn save_watermark(path: Option<&Path>) -> Result<(), String> {
    for_user()?.save_watermark(path)
}

#[cfg(windows)]
pub(crate) fn load_signature_image() -> Result<Option<PathBuf>, String> {
    for_user()?.load_signature_image()
}

#[cfg(windows)]
pub(crate) fn save_signature_image(path: Option<&Path>) -> Result<(), String> {
    for_user()?.save_signature_image(path)
}

#[cfg(windows)]
pub(crate) fn load_identity() -> Result<Option<Identity>, String> {
    for_user()?.load_identity()
}

#[cfg(windows)]
pub(crate) fn save_identity(identity: Option<&Identity>) -> Result<(), String> {
    for_user()?.save_identity(identity)
}

#[cfg(windows)]
pub(crate) fn restore_tabs_enabled() -> bool {
    for_user()
        .and_then(|prefs| prefs.restore_tabs_enabled())
        .unwrap_or(true)
}

#[cfg(windows)]
pub(crate) fn set_restore_tabs_enabled(enabled: bool) -> Result<(), String> {
    for_user()?.set_restore_tabs_enabled(enabled)
}

#[cfg(windows)]
pub(crate) fn load_tab_session() -> Result<TabSession, String> {
    for_user()?.load_tab_session()
}

#[cfg(windows)]
pub(crate) fn save_tab_session(session: &TabSession) -> Result<(), String> {
    for_user()?.save_tab_session(session)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(profile: &Path) -> Preferences {
        Preferences::from_profile(Some(profile.as_os_str().to_owned())).unwrap()
    }

    #[test]
    fn tab_session_skips_missing_files_and_honors_the_persisted_switch() {
        let profile = tempfile::tempdir().unwrap();
        let prefs = settings(profile.path());
        let present = profile.path().join("論文 1.pdf").into_os_string();
        let missing = profile.path().join("moved.pdf").into_os_string();
        fs::write(Path::new(&present), "").unwrap();
        assert!(prefs.restore_tabs_enabled().unwrap());

        let session = TabSession {
            active: Some(present.clone()),
            tabs: vec![
                RememberedTab {
                    path: missing,
                    page: 0,
                },
                RememberedTab {
                    path: present.clone(),
                    page: 4,
                },
            ],
        };
        prefs.save_tab_session(&session).unwrap();
        let next_launch = settings(profile.path());
        let restored = next_launch.load_tab_session().unwrap();
        assert_eq!(restored.active, Some(present.clone()));
        assert_eq!(
            restored.tabs,
            [RememberedTab {
                path: present,
                page: 4
            }]
        );

        next_launch.set_restore_tabs_enabled(false).unwrap();
        assert!(!prefs.restore_tabs_enabled().unwrap());
        prefs.save_tab_session(&TabSession::default()).unwrap();
        assert!(prefs.load_tab_session().unwrap().tabs.is_empty());
        next_launch.set_restore_tabs_enabled(true).unwrap();
        assert_eq!(prefs.load_tab_session().unwrap(), restored);
    }

    #[test]
    fn restore_replace_and_forget_independent_preferences() {
        let profile = tempfile::tempdir().unwrap();
        let prefs = settings(profile.path());
        let watermark = profile.path().join("押印 🌸.png");
        let signature_image = profile.path().join("手書きサイン.jpg");
        let pfx_path = profile.path().join("署名用.pfx");
        assert!(prefs.load_watermark(|| None).unwrap().is_none());
        assert!(prefs.load_signature_image().unwrap().is_none());
        assert!(prefs.load_identity().unwrap().is_none());
        prefs.save_identity(None).unwrap();
        assert!(!prefs.0.exists());

        prefs.save_watermark(Some(&watermark)).unwrap();
        prefs.save_signature_image(Some(&signature_image)).unwrap();
        let pfx = PfxIdentity {
            path: pfx_path.clone(),
            name: "署名者".to_owned(),
            thumbprint: [123; 20],
            file_hash: [81; 32],
            protected_password: Some(vec![43, 29]),
        };
        prefs.save_identity(Some(&Identity::Pfx(pfx))).unwrap();
        let restored = settings(profile.path());
        assert_eq!(
            restored.load_watermark(|| None).unwrap(),
            Some(watermark.clone())
        );
        let Some(Identity::Pfx(pfx)) = restored.load_identity().unwrap() else {
            panic!()
        };
        assert_eq!((pfx.path, pfx.name.as_str()), (pfx_path, "署名者"));
        assert_eq!(pfx.thumbprint, [123; 20]);
        assert_eq!(pfx.file_hash, [81; 32]);
        assert_eq!(pfx.protected_password, Some(vec![43, 29]));

        prefs
            .save_identity(Some(&Identity::Store {
                name: "新しい署名者".to_owned(),
                thumbprint: [124; 20],
            }))
            .unwrap();
        prefs.save_watermark(None).unwrap();
        assert!(
            restored
                .load_watermark(|| Some(watermark.clone()))
                .unwrap()
                .is_none()
        );
        let Some(Identity::Store { name, thumbprint }) = restored.load_identity().unwrap() else {
            panic!()
        };
        assert_eq!(name, "新しい署名者");
        assert_eq!(thumbprint, [124; 20]);
        assert_eq!(
            restored.load_signature_image().unwrap(),
            Some(signature_image)
        );
        prefs.save_watermark(Some(&watermark)).unwrap();
        prefs.save_signature_image(None).unwrap();
        assert!(restored.load_signature_image().unwrap().is_none());
        prefs.save_identity(None).unwrap();
        assert!(restored.load_identity().unwrap().is_none());
        assert_eq!(restored.load_watermark(|| None).unwrap(), Some(watermark));
    }

    #[test]
    fn corrupt_and_oversized_records_can_be_replaced_explicitly() {
        let profile = tempfile::tempdir().unwrap();
        let prefs = settings(profile.path());
        fs::create_dir(&prefs.0).unwrap();
        let identity_file = prefs.0.join(IDENTITY_FILE);
        let bad_identity = br#"{"kind":"store","name":"test","thumbprint":[0]}"#;
        fs::write(&identity_file, bad_identity).unwrap();
        let watermark_file = prefs.0.join(WATERMARK_FILE);
        fs::write(&watermark_file, vec![b' '; MAX_RECORD_BYTES + 1]).unwrap();
        assert!(prefs.load_identity().is_err());
        assert!(prefs.load_watermark(|| None).is_err());
        assert_eq!(fs::read(&identity_file).unwrap(), bad_identity);
        assert_eq!(
            fs::metadata(&watermark_file).unwrap().len(),
            MAX_RECORD_BYTES as u64 + 1
        );

        let watermark = profile.path().join("new.png");
        prefs.save_watermark(Some(&watermark)).unwrap();
        assert_eq!(
            prefs.load_watermark(|| None).unwrap().as_deref(),
            Some(watermark.as_path())
        );
        assert!(prefs.load_identity().is_err());
        assert!(
            prefs
                .save_watermark(Some(Path::new("relative.png")))
                .is_err()
        );
        assert_eq!(prefs.load_watermark(|| None).unwrap(), Some(watermark));
        prefs.save_identity(None).unwrap();
        assert!(prefs.load_identity().unwrap().is_none());
    }

    #[test]
    fn requires_an_absolute_profile_directory() {
        assert!(Preferences::from_profile(None).is_err());
        assert!(Preferences::from_profile(Some("".into())).is_err());
        assert!(Preferences::from_profile(Some("relative".into())).is_err());
    }
}
