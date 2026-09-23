mod product_operations;
mod protection;
mod selection;

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub use product_operations::{
    DiagnosticExportSummary, RetentionApplication, RetentionCandidate, RetentionPlan,
    apply_retention_plan, create_diagnostic_export, create_retention_plan,
};
pub use selection::{ObserverSelection, select_observer_runs};

const PRODUCT_CONFIGURATION_SCHEMA_VERSION: &str = "0.1.0";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RawEvidenceAccess {
    CurrentWindowsUser,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticExportMode {
    ExplicitUserAction,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProductConfiguration {
    pub schema_version: String,
    pub evidence_root: PathBuf,
    pub mcp_manifest: PathBuf,
    pub diagnostic_export_root: PathBuf,
    pub retention_days: u16,
    pub raw_evidence_access: RawEvidenceAccess,
    pub diagnostic_export_mode: DiagnosticExportMode,
}

impl ProductConfiguration {
    pub fn new(
        evidence_root: PathBuf,
        mcp_manifest: PathBuf,
        diagnostic_export_root: PathBuf,
        retention_days: u16,
    ) -> Result<Self, String> {
        if retention_days == 0 {
            return Err(String::from("证据保留天数必须大于 0"));
        }
        Ok(Self {
            schema_version: String::from(PRODUCT_CONFIGURATION_SCHEMA_VERSION),
            evidence_root,
            mcp_manifest,
            diagnostic_export_root,
            retention_days,
            raw_evidence_access: RawEvidenceAccess::CurrentWindowsUser,
            diagnostic_export_mode: DiagnosticExportMode::ExplicitUserAction,
        })
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != PRODUCT_CONFIGURATION_SCHEMA_VERSION {
            return Err(format!(
                "不支持的产品配置版本 expected={} actual={}",
                PRODUCT_CONFIGURATION_SCHEMA_VERSION, self.schema_version
            ));
        }
        if self.retention_days == 0 {
            return Err(String::from("证据保留天数必须大于 0"));
        }
        if !self.evidence_root.is_dir() {
            return Err(format!(
                "产品证据根目录不存在 path={}",
                self.evidence_root.display()
            ));
        }
        if !self.mcp_manifest.is_file() {
            return Err(format!(
                "MCP manifest 不存在 path={}",
                self.mcp_manifest.display()
            ));
        }
        Ok(())
    }
}

pub fn save_encrypted(path: &Path, configuration: &ProductConfiguration) -> Result<(), String> {
    configuration.validate()?;
    let encoded = serde_json::to_vec(configuration)
        .map_err(|error| format!("产品配置序列化失败 error={error}"))?;
    let protected = protection::protect(&encoded)?;
    let parent = path
        .parent()
        .ok_or_else(|| format!("产品配置路径缺少父目录 path={}", path.display()))?;
    fs::create_dir_all(parent).map_err(|error| {
        format!(
            "无法创建产品配置目录 path={} error={error}",
            parent.display()
        )
    })?;
    let temporary_path = path.with_extension("tmp");
    fs::write(&temporary_path, protected).map_err(|error| {
        format!(
            "无法写入产品配置临时文件 path={} error={error}",
            temporary_path.display()
        )
    })?;
    fs::rename(&temporary_path, path).map_err(|error| {
        format!(
            "无法原子发布产品配置 source={} target={} error={error}",
            temporary_path.display(),
            path.display()
        )
    })
}

pub fn load_encrypted(path: &Path) -> Result<ProductConfiguration, String> {
    let protected = fs::read(path)
        .map_err(|error| format!("无法读取产品配置 path={} error={error}", path.display()))?;
    let encoded = protection::unprotect(&protected)?;
    let configuration = serde_json::from_slice::<ProductConfiguration>(&encoded)
        .map_err(|error| format!("产品配置解析失败 path={} error={error}", path.display()))?;
    configuration.validate()?;
    Ok(configuration)
}
