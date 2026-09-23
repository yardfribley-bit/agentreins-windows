use std::time::{SystemTime, UNIX_EPOCH};

use product_config::{
    DiagnosticExportSummary, ObserverSelection, ProductConfiguration, RetentionApplication,
    RetentionPlan, apply_retention_plan, create_diagnostic_export, create_retention_plan,
    select_observer_runs,
};
use serde::Serialize;

#[derive(Clone)]
pub struct ProductService {
    configuration: ProductConfiguration,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProductSettingsView {
    pub retention_days: u16,
    pub raw_evidence_access: &'static str,
    pub diagnostic_export_mode: &'static str,
    pub diagnostic_export_root: String,
    pub selected_session_id: String,
}

impl ProductService {
    pub fn new(configuration: ProductConfiguration) -> Result<Self, String> {
        configuration.validate()?;
        select_observer_runs(&configuration)?;
        Ok(Self { configuration })
    }

    pub fn current_selection(&self) -> Result<ObserverSelection, String> {
        select_observer_runs(&self.configuration)
    }

    pub fn settings(&self) -> Result<ProductSettingsView, String> {
        let selection = self.current_selection()?;
        Ok(ProductSettingsView {
            retention_days: self.configuration.retention_days,
            raw_evidence_access: "current_windows_user",
            diagnostic_export_mode: "explicit_user_action",
            diagnostic_export_root: self
                .configuration
                .diagnostic_export_root
                .display()
                .to_string(),
            selected_session_id: selection.session_id,
        })
    }

    pub fn retention_plan(&self) -> Result<RetentionPlan, String> {
        let selection = self.current_selection()?;
        create_retention_plan(&self.configuration, &selection, current_unix_ms()?)
    }

    pub fn apply_retention(
        &self,
        confirmation_token: &str,
    ) -> Result<RetentionApplication, String> {
        let selection = self.current_selection()?;
        apply_retention_plan(
            &self.configuration,
            &selection,
            confirmation_token,
            current_unix_ms()?,
        )
    }

    pub fn create_diagnostic_export(
        &self,
        archive_name: &str,
    ) -> Result<DiagnosticExportSummary, String> {
        let selection = self.current_selection()?;
        create_diagnostic_export(&self.configuration, &selection, archive_name)
    }
}

fn current_unix_ms() -> Result<u64, String> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("当前时间早于 Unix 元年 error={error}"))?;
    u64::try_from(duration.as_millis())
        .map_err(|error| format!("当前 Unix 毫秒超出 u64 error={error}"))
}
