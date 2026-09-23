use std::collections::HashSet;

use native_contracts::{AgentKind, Confidence, IdentityStatus};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AgentMatch {
    pub agent_kind: AgentKind,
    pub identity_status: IdentityStatus,
    pub confidence: Confidence,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessObservation<'a> {
    pub pid: u32,
    pub parent_pid: Option<u32>,
    pub image_name: &'a str,
    pub identity_verified: bool,
}

pub fn classify_process(
    tracked_process_ids: &HashSet<u32>,
    observation: &ProcessObservation<'_>,
) -> Option<AgentMatch> {
    if observation
        .parent_pid
        .is_some_and(|parent_pid| tracked_process_ids.contains(&parent_pid))
    {
        return Some(AgentMatch {
            agent_kind: AgentKind::WorkBuddy,
            identity_status: IdentityStatus::Inherited,
            confidence: Confidence::High,
        });
    }

    if is_workbuddy_executable(observation.image_name) {
        return Some(AgentMatch {
            agent_kind: AgentKind::WorkBuddy,
            identity_status: if observation.identity_verified {
                IdentityStatus::Verified
            } else {
                IdentityStatus::Candidate
            },
            confidence: if observation.identity_verified {
                Confidence::High
            } else {
                Confidence::Medium
            },
        });
    }

    None
}

fn is_workbuddy_executable(image_name: &str) -> bool {
    image_name
        .rsplit(['\\', '/'])
        .next()
        .is_some_and(|file_name| file_name.eq_ignore_ascii_case("WorkBuddy.exe"))
}

#[cfg(test)]
mod tests {
    use super::{ProcessObservation, classify_process};
    use native_contracts::{Confidence, IdentityStatus};
    use std::collections::HashSet;

    #[test]
    fn executable_name_identifies_workbuddy_candidate() {
        let result = classify_process(
            &HashSet::new(),
            &ProcessObservation {
                pid: 100,
                parent_pid: Some(10),
                image_name: r"C:\Program Files\WorkBuddy\WorkBuddy.exe",
                identity_verified: false,
            },
        )
        .expect("WorkBuddy.exe 应被识别");

        assert_eq!(result.identity_status, IdentityStatus::Candidate);
        assert_eq!(result.confidence, Confidence::Medium);
    }

    #[test]
    fn verified_executable_path_promotes_workbuddy_identity() {
        let result = classify_process(
            &HashSet::new(),
            &ProcessObservation {
                pid: 100,
                parent_pid: Some(10),
                image_name: r"C:\Program Files\WorkBuddy\WorkBuddy.exe",
                identity_verified: true,
            },
        )
        .expect("已验证 WorkBuddy.exe 应被识别");

        assert_eq!(result.identity_status, IdentityStatus::Verified);
        assert_eq!(result.confidence, Confidence::High);
    }

    #[test]
    fn child_process_inherits_workbuddy_identity() {
        let result = classify_process(
            &HashSet::from([100]),
            &ProcessObservation {
                pid: 101,
                parent_pid: Some(100),
                image_name: r"C:\Windows\System32\cmd.exe",
                identity_verified: false,
            },
        )
        .expect("WorkBuddy 子进程应继承身份");

        assert_eq!(result.identity_status, IdentityStatus::Inherited);
        assert_eq!(result.confidence, Confidence::High);
    }

    #[test]
    fn same_executable_child_inherits_existing_root() {
        let result = classify_process(
            &HashSet::from([100]),
            &ProcessObservation {
                pid: 101,
                parent_pid: Some(100),
                image_name: r"C:\Program Files\WorkBuddy\WorkBuddy.exe",
                identity_verified: true,
            },
        )
        .expect("WorkBuddy 同名子进程应继承现有根实例");

        assert_eq!(result.identity_status, IdentityStatus::Inherited);
        assert_eq!(result.confidence, Confidence::High);
    }

    #[test]
    fn unrelated_process_is_not_classified() {
        let result = classify_process(
            &HashSet::new(),
            &ProcessObservation {
                pid: 200,
                parent_pid: Some(10),
                image_name: r"C:\Windows\System32\notepad.exe",
                identity_verified: false,
            },
        );

        assert_eq!(result, None);
    }

    #[test]
    fn bare_tracked_pid_does_not_survive_pid_reuse() {
        let result = classify_process(
            &HashSet::from([101]),
            &ProcessObservation {
                pid: 101,
                parent_pid: None,
                image_name: r"C:\Windows\System32\cmd.exe",
                identity_verified: false,
            },
        );

        assert_eq!(result, None);
    }
}
