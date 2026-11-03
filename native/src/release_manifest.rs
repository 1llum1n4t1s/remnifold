//! R2 配布の共有形式。JSON の実バイト列は GitHub の anchor で別途認証する。
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const REPOSITORY: &str = "1llum1n4t1s/remnifold";
pub const PUBLIC_BASE_URL: &str = "https://remnifold-updates.kagayoi.com";
pub const STATUS_CONTEXT: &str = "remnifold/r2/v1";
pub fn status_url(run: u64, attempt: u64) -> String {
    format!("https://github.com/{REPOSITORY}/actions/runs/{run}/attempts/{attempt}")
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DistributionConfig {
    pub schema: u32,
    pub repository: String,
    pub staging_bucket: String,
    pub public_bucket: String,
    pub public_base_url: String,
    pub status_context: String,
    pub workflow_path: String,
}
pub fn parse_config(bytes: &[u8]) -> Result<DistributionConfig> {
    let c: DistributionConfig = parse(bytes)?;
    ensure!(
        c.schema == 1
            && c.repository == REPOSITORY
            && c.staging_bucket == "remnifold-builds"
            && c.public_bucket == "remnifold-updates"
            && c.public_base_url == PUBLIC_BASE_URL
            && c.status_context == STATUS_CONTEXT
            && c.workflow_path == ".github/workflows/ci.yml",
        "Invalid distribution configuration"
    );
    Ok(c)
}
/// HTTP loopback は更新テスト専用。配信側は常に parse_config を使う。
pub fn parse_updater_config(bytes: &[u8]) -> Result<DistributionConfig> {
    let mut c: DistributionConfig = parse(bytes)?;
    let original = c.public_base_url.clone();
    if original != PUBLIC_BASE_URL {
        let u = url::Url::parse(&original)?;
        ensure!(
            u.scheme() == "http"
                && u.host_str() == Some("127.0.0.1")
                && u.port().is_some()
                && u.username().is_empty()
                && u.password().is_none()
                && u.path() == "/"
                && u.query().is_none()
                && u.fragment().is_none(),
            "Invalid updater test origin"
        );
        c.public_base_url = PUBLIC_BASE_URL.into();
    }
    parse_config(&serde_json::to_vec(&c)?)?;
    c.public_base_url = original;
    Ok(c)
}
pub const MAX_JSON: usize = 32 * 1024;
pub const MAX_ARCHIVE: u64 = 128 * 1024 * 1024;
pub const MAX_EXPANDED: u64 = 128 * 1024 * 1024;
pub const TARGETS: [&str; 5] = [
    "x86_64-pc-windows-msvc",
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
];

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReleaseAsset {
    pub key: String,
    pub size: u64,
    pub sha256: String,
    pub native_manifest_sha256: String,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReleaseManifest {
    pub schema: u32,
    pub repository: String,
    pub version: String,
    pub source_commit: String,
    pub run_id: u64,
    pub run_attempt: u64,
    pub targets: BTreeMap<String, ReleaseAsset>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VersionIndex {
    pub schema: u32,
    pub repository: String,
    pub version: String,
    pub source_commit: String,
    pub run_id: u64,
    pub run_attempt: u64,
    pub manifest_key: String,
    pub manifest_sha256: String,
}
pub fn hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub fn version(value: &str) -> bool {
    let (value, build) = value
        .split_once('+')
        .map_or((value, None), |(a, b)| (a, Some(b)));
    let identifiers = |value: &str, numeric_guard: bool| {
        value.split('.').all(|part| {
            !part.is_empty()
                && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                && (!numeric_guard
                    || !part.bytes().all(|b| b.is_ascii_digit())
                    || part == "0"
                    || !part.starts_with('0'))
        })
    };
    if !build.is_none_or(|s| identifiers(s, false)) {
        return false;
    }
    let (core, pre) = value
        .split_once('-')
        .map_or((value, None), |(a, b)| (a, Some(b)));
    core.split('.').count() == 3
        && core.split('.').all(|part| {
            !part.is_empty()
                && part.bytes().all(|b| b.is_ascii_digit())
                && (part == "0" || !part.starts_with('0'))
        })
        && pre.is_none_or(|s| identifiers(s, true))
}
fn identity(
    schema: u32,
    repository: &str,
    ver: &str,
    commit: &str,
    run: u64,
    attempt: u64,
) -> Result<()> {
    ensure!(
        schema == 1
            && repository == REPOSITORY
            && version(ver)
            && hex(commit, 40)
            && run > 0
            && attempt > 0,
        "Invalid release identity"
    );
    Ok(())
}
pub fn index_key(version: &str) -> String {
    format!("releases/{version}/index.json")
}
pub fn manifest_key(version: &str, commit: &str, run: u64, attempt: u64) -> String {
    format!("releases/{version}/{commit}/{run}-{attempt}/manifest.json")
}
pub fn asset_key(
    version: &str,
    commit: &str,
    run: u64,
    attempt: u64,
    target: &str,
    digest: &str,
) -> String {
    format!("releases/{version}/{commit}/{run}-{attempt}/{target}/{digest}.tar.gz")
}
impl ReleaseManifest {
    pub fn validate(&self) -> Result<()> {
        identity(
            self.schema,
            &self.repository,
            &self.version,
            &self.source_commit,
            self.run_id,
            self.run_attempt,
        )?;
        ensure!(
            self.targets.len() == TARGETS.len(),
            "Incomplete release targets"
        );
        for target in TARGETS {
            let a = self
                .targets
                .get(target)
                .ok_or_else(|| anyhow::anyhow!("Missing release target"))?;
            ensure!(
                a.size > 0
                    && a.size <= MAX_ARCHIVE
                    && hex(&a.sha256, 64)
                    && hex(&a.native_manifest_sha256, 64),
                "Invalid release asset"
            );
            ensure!(
                a.key
                    == asset_key(
                        &self.version,
                        &self.source_commit,
                        self.run_id,
                        self.run_attempt,
                        target,
                        &a.sha256
                    ),
                "Invalid release asset key"
            );
        }
        Ok(())
    }
}
impl VersionIndex {
    pub fn validate(&self) -> Result<()> {
        identity(
            self.schema,
            &self.repository,
            &self.version,
            &self.source_commit,
            self.run_id,
            self.run_attempt,
        )?;
        ensure!(
            hex(&self.manifest_sha256, 64)
                && self.manifest_key
                    == manifest_key(
                        &self.version,
                        &self.source_commit,
                        self.run_id,
                        self.run_attempt
                    ),
            "Invalid release index"
        );
        Ok(())
    }
}
// serde's map visitor rejects duplicate fields, including nested target names.
struct Unique(serde_json::Value);
impl<'de> Deserialize<'de> for Unique {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = Unique;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("JSON without duplicate keys")
            }
            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                mut m: M,
            ) -> std::result::Result<Unique, M::Error> {
                let mut out = serde_json::Map::new();
                while let Some((k, v)) = m.next_entry::<String, Unique>()? {
                    if out.insert(k, v.0).is_some() {
                        return Err(serde::de::Error::custom("Duplicate JSON key"));
                    }
                }
                Ok(Unique(out.into()))
            }
            fn visit_seq<S: serde::de::SeqAccess<'de>>(
                self,
                mut s: S,
            ) -> std::result::Result<Unique, S::Error> {
                let mut out = Vec::new();
                while let Some(v) = s.next_element::<Unique>()? {
                    out.push(v.0);
                }
                Ok(Unique(out.into()))
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> std::result::Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_bool<E: serde::de::Error>(self, v: bool) -> std::result::Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> std::result::Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> std::result::Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> std::result::Result<Unique, E> {
                serde_json::Number::from_f64(v)
                    .map(|n| Unique(n.into()))
                    .ok_or_else(|| E::custom("Invalid number"))
            }
            fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<Unique, E> {
                Ok(Unique(serde_json::Value::Null))
            }
        }
        d.deserialize_any(V)
    }
}
pub fn parse_strict<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    ensure!(bytes.len() <= MAX_JSON, "Release metadata too large");
    let unique: Unique = serde_json::from_slice(bytes)?;
    Ok(serde_json::from_value(unique.0)?)
}
fn parse<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    parse_strict(bytes)
}
pub fn parse_manifest(bytes: &[u8]) -> Result<ReleaseManifest> {
    let v: ReleaseManifest = parse(bytes)?;
    v.validate()?;
    Ok(v)
}
pub fn parse_index(bytes: &[u8]) -> Result<VersionIndex> {
    let v: VersionIndex = parse(bytes)?;
    v.validate()?;
    Ok(v)
}
pub const MANIFEST_MARKER: &str = "REMNIFOLD_R2_MANIFEST_V1";
pub const ASSET_MARKER: &str = "REMNIFOLD_R2_ASSET_V1";
pub const CANDIDATE_MARKER: &str = "REMNIFOLD_R2_CANDIDATE_V1";
pub const MAX_WITNESS_LOG: usize = 4 * 1024 * 1024;
pub fn witness_lines(manifest: &ReleaseManifest, digest: &str) -> Result<Vec<String>> {
    manifest.validate()?;
    ensure!(hex(digest, 64), "Invalid witness digest");
    let mut lines = vec![format!(
        "{MANIFEST_MARKER} {} {} {} {} {digest}",
        manifest.version, manifest.source_commit, manifest.run_id, manifest.run_attempt
    )];
    for target in TARGETS {
        let asset = &manifest.targets[target];
        lines.push(format!(
            "{ASSET_MARKER} {target} {} {} {}",
            asset.sha256, asset.size, asset.key
        ));
    }
    ensure!(
        lines.iter().all(|line| line.is_ascii()),
        "Invalid witness text"
    );
    Ok(lines)
}
pub fn candidate_line(manifest: &ReleaseManifest, digest: &str) -> Result<String> {
    manifest.validate()?;
    ensure!(hex(digest, 64), "Invalid candidate digest");
    Ok(format!(
        "{CANDIDATE_MARKER} {} {} {} {} {digest}",
        manifest.version, manifest.source_commit, manifest.run_id, manifest.run_attempt
    ))
}
pub fn verify_candidate(log: &[u8], manifest: &ReleaseManifest, digest: &str) -> Result<()> {
    ensure!(log.len() <= MAX_WITNESS_LOG, "R2 candidate log too large");
    let expected = candidate_line(manifest, digest)?;
    let mut found = false;
    for raw in std::str::from_utf8(log)?.lines() {
        if !raw.contains("REMNIFOLD_R2_CANDIDATE") {
            continue;
        }
        let line = raw
            .split_once(' ')
            .filter(|(prefix, _)| log_timestamp(prefix))
            .map(|(_, tail)| tail)
            .unwrap_or(raw);
        ensure!(
            line == expected && !found,
            "Invalid or duplicate R2 candidate witness"
        );
        found = true;
    }
    ensure!(found, "Missing R2 candidate witness");
    Ok(())
}
fn log_timestamp(value: &str) -> bool {
    let b = value.as_bytes();
    if !(20..=40).contains(&b.len())
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b.last() != Some(&b'Z')
    {
        return false;
    }
    if !(0..19)
        .filter(|i| ![4, 7, 10, 13, 16].contains(i))
        .all(|i| b[i].is_ascii_digit())
    {
        return false;
    }
    b.len() == 20
        || (b[19] == b'.' && b.len() > 21 && b[20..b.len() - 1].iter().all(|v| v.is_ascii_digit()))
}
/// GitHub の job log にある、ただ一組の公開証明だけを認める。
pub fn verify_witness(log: &[u8], manifest: &ReleaseManifest, digest: &str) -> Result<()> {
    ensure!(log.len() <= MAX_WITNESS_LOG, "R2 witness log too large");
    let expected = witness_lines(manifest, digest)?;
    let mut seen = std::collections::BTreeSet::new();
    for raw in std::str::from_utf8(log)?.lines() {
        if !raw.contains("REMNIFOLD_R2_MANIFEST") && !raw.contains("REMNIFOLD_R2_ASSET") {
            continue;
        }
        let line = raw
            .split_once(' ')
            .filter(|(prefix, _)| log_timestamp(prefix))
            .map(|(_, tail)| tail)
            .unwrap_or(raw);
        let position = expected
            .iter()
            .position(|value| value == line)
            .ok_or_else(|| anyhow::anyhow!("Invalid R2 publication witness"))?;
        ensure!(seen.insert(position), "Duplicate R2 publication witness");
    }
    ensure!(
        seen.len() == expected.len(),
        "Incomplete R2 publication witness"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_duplicate_and_unsafe_identity() {
        assert!(parse_index(br#"{"schema":1,"schema":1}"#).is_err());
        assert!(!version("../1"));
        assert!(!hex(&"A".repeat(40), 40));
        assert!(version("1.0.15"));
        assert!(version("1.0.16-rc.1"));
        assert!(version("1.0.16+build.1"));
        assert!(version("123456789012345.0.1"));
        assert!(!version("1.0.16-01"));
        assert!(!version("1.0.16+"));
    }
    fn complete() -> ReleaseManifest {
        let mut m = ReleaseManifest {
            schema: 1,
            repository: REPOSITORY.into(),
            version: "1.0.15".into(),
            source_commit: "a".repeat(40),
            run_id: 7,
            run_attempt: 2,
            targets: BTreeMap::new(),
        };
        for target in TARGETS {
            let digest = "b".repeat(64);
            m.targets.insert(
                target.into(),
                ReleaseAsset {
                    key: asset_key(
                        &m.version,
                        &m.source_commit,
                        m.run_id,
                        m.run_attempt,
                        target,
                        &digest,
                    ),
                    size: 42,
                    sha256: digest,
                    native_manifest_sha256: "c".repeat(64),
                },
            );
        }
        m
    }
    #[test]
    fn exact_targets_and_bound_keys_are_required() {
        let m = complete();
        assert_eq!(parse_manifest(&serde_json::to_vec(&m).unwrap()).unwrap(), m);
        let mut bad = m.clone();
        bad.targets.remove(TARGETS[0]);
        assert!(bad.validate().is_err());
        let mut bad = m.clone();
        bad.targets
            .insert("unknown".into(), bad.targets[TARGETS[0]].clone());
        assert!(bad.validate().is_err());
        let mut bad = m.clone();
        bad.targets.get_mut(TARGETS[0]).unwrap().key = "../escape".into();
        assert!(bad.validate().is_err());
        let mut bad = m.clone();
        bad.targets.get_mut(TARGETS[0]).unwrap().size = MAX_ARCHIVE + 1;
        assert!(bad.validate().is_err());
        let mut bad = m.clone();
        bad.run_attempt = 0;
        assert!(bad.validate().is_err());
        let mut bad = m;
        bad.repository = "foreign/repository".into();
        assert!(bad.validate().is_err());
    }
    #[test]
    fn duplicate_unknown_and_oversized_json_is_rejected() {
        let m = complete();
        let json = serde_json::to_string(&m).unwrap();
        let target = TARGETS[0];
        let value = serde_json::to_string(&m.targets[target]).unwrap();
        let duplicated = json.replace(
            &format!("\"{target}\":{value}"),
            &format!("\"{target}\":{value},\"{target}\":{value}"),
        );
        assert!(parse_manifest(duplicated.as_bytes()).is_err());
        let unknown = json.replacen('{', "{\"unknown\":1,", 1);
        assert!(parse_manifest(unknown.as_bytes()).is_err());
        assert!(parse_manifest(&vec![b' '; MAX_JSON + 1]).is_err());
        let mut index = VersionIndex {
            schema: 1,
            repository: REPOSITORY.into(),
            version: m.version.clone(),
            source_commit: m.source_commit.clone(),
            run_id: m.run_id,
            run_attempt: m.run_attempt,
            manifest_key: manifest_key(&m.version, &m.source_commit, m.run_id, m.run_attempt),
            manifest_sha256: "d".repeat(64),
        };
        assert!(parse_index(&serde_json::to_vec(&index).unwrap()).is_ok());
        index.manifest_key.push_str("?token=secret");
        assert!(index.validate().is_err());
    }
    #[test]
    fn publication_witness_requires_one_exact_complete_marker_set() {
        let m = complete();
        let digest = "d".repeat(64);
        let lines = witness_lines(&m, &digest).unwrap();
        assert_eq!(lines.len(), 6);
        let log = lines
            .iter()
            .map(|line| format!("2026-11-03T12:34:56.1234567Z {line}\r\n"))
            .collect::<String>();
        verify_witness(log.as_bytes(), &m, &digest).unwrap();
        assert!(verify_witness(format!("{log}{}\n", lines[0]).as_bytes(), &m, &digest).is_err());
        assert!(verify_witness(lines[..5].join("\n").as_bytes(), &m, &digest).is_err());
        assert!(verify_witness(log.replace(" 7 2 ", " 7 3 ").as_bytes(), &m, &digest).is_err());
        assert!(
            verify_witness(
                format!("{log}REMNIFOLD_R2_ASSET_V1 partial\n").as_bytes(),
                &m,
                &digest
            )
            .is_err()
        );
        assert!(
            verify_witness(
                log.replace("2026-11-03T12:34:56.1234567Z ", "untrusted-prefix ")
                    .as_bytes(),
                &m,
                &digest
            )
            .is_err()
        );
        assert!(verify_witness(&vec![b' '; MAX_WITNESS_LOG + 1], &m, &digest).is_err());
    }
    #[test]
    fn provisional_candidate_requires_one_exact_origin_marker() {
        let m = complete();
        let digest = "d".repeat(64);
        let line = candidate_line(&m, &digest).unwrap();
        verify_candidate(
            format!("2026-11-03T12:34:56Z {line}\n").as_bytes(),
            &m,
            &digest,
        )
        .unwrap();
        assert!(verify_candidate(format!("{line}\n{line}\n").as_bytes(), &m, &digest).is_err());
        assert!(verify_candidate(b"REMNIFOLD_R2_CANDIDATE_V1 partial\n", &m, &digest).is_err());
        assert!(verify_candidate(&[], &m, &digest).is_err());
        assert!(verify_candidate(line.replace(" 7 2 ", " 7 3 ").as_bytes(), &m, &digest).is_err());
    }
}
