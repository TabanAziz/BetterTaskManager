//! Display names of packaged (Store) apps of the current user.

use std::collections::HashMap;

use windows_sys::Win32::UI::Shell::SHLoadIndirectString;

use crate::reg;
use crate::util::{from_wide, wide};

const REPOSITORY: &str =
    "Software\\Classes\\Local Settings\\Software\\Microsoft\\Windows\\CurrentVersion\\AppModel\\Repository\\Packages";

pub struct Packages {
    /// Lower-case family name → full names.
    families: HashMap<String, Vec<String>>,
}

pub struct Package {
    pub name: String,
    pub publisher: String,
}

/// "Name_1.0_x64__hash" → "name_hash".
fn family_of(full: &str) -> Option<String> {
    let name = full.split('_').next()?;
    let hash = full.rsplit('_').next()?;
    (name != full).then(|| format!("{name}_{hash}").to_lowercase())
}

fn resolve(text: String) -> Option<String> {
    if !text.starts_with('@') {
        return (!text.is_empty() && !text.starts_with("ms-resource:")).then_some(text);
    }
    let source = wide(&text);
    let mut out = [0u16; 512];
    let hr = unsafe { SHLoadIndirectString(source.as_ptr(), out.as_mut_ptr(), out.len() as u32, std::ptr::null_mut()) };
    (hr >= 0).then(|| from_wide(&out)).filter(|s| !s.is_empty())
}

fn between<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let rest = &text[text.find(open)? + open.len()..];
    Some(&rest[..rest.find(close)?])
}

impl Packages {
    pub fn scan() -> Self {
        let mut families: HashMap<String, Vec<String>> = HashMap::new();
        for full in reg::subkeys(reg::USER, REPOSITORY) {
            if let Some(family) = family_of(&full) {
                families.entry(family).or_default().push(full);
            }
        }
        Self { families }
    }

    pub fn knows(&self, family: &str) -> bool {
        self.families.contains_key(&family.to_lowercase())
    }

    pub fn describe(&self, family: &str) -> Package {
        let fulls = self.families.get(&family.to_lowercase()).map(Vec::as_slice).unwrap_or_default();
        let mut package = Package { name: String::new(), publisher: String::new() };
        for full in fulls {
            let key = format!("{REPOSITORY}\\{full}");
            let Some(name) = reg::string(reg::USER, &key, "DisplayName").and_then(resolve) else { continue };
            package.name = name;
            if let Some(root) = reg::string(reg::USER, &key, "PackageRootFolder") {
                let manifest = std::fs::read_to_string(format!("{root}\\AppxManifest.xml")).unwrap_or_default();
                package.publisher = between(&manifest, "<PublisherDisplayName>", "</PublisherDisplayName>")
                    .filter(|p| !p.starts_with("ms-resource:"))
                    .unwrap_or_default()
                    .replace("&amp;", "&");
            }
            return package;
        }
        // "Microsoft.WindowsTerminal_8wekyb3d8bbwe" → "WindowsTerminal"
        let plain = family.split('_').next().unwrap_or(family);
        package.name = plain.rsplit('.').next().unwrap_or(plain).to_string();
        package
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn family_from_full_name() {
        let family = super::family_of("Contoso.App_1.2.3.0_x64__8wekyb3d8bbwe");
        assert_eq!(family.as_deref(), Some("contoso.app_8wekyb3d8bbwe"));
        assert_eq!(super::family_of("nonsense"), None);
    }
}
