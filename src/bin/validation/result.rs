// TestResult - validation sonuçlarını temsil eder

#[derive(Debug, Clone)]
pub struct TestResult {
    pub name: String,
    pub success: bool,
    pub message: String,
    pub details: Option<String>,
}

impl TestResult {
    pub fn success(name: &str, message: &str) -> Self {
        Self {
            name: name.to_string(),
            success: true,
            message: message.to_string(),
            details: None,
        }
    }

    pub fn success_with_details(name: &str, message: &str, details: String) -> Self {
        Self {
            name: name.to_string(),
            success: true,
            message: message.to_string(),
            details: Some(details),
        }
    }

    pub fn failure(name: &str, message: &str) -> Self {
        Self {
            name: name.to_string(),
            success: false,
            message: message.to_string(),
            details: None,
        }
    }

    pub fn failure_with_details(name: &str, message: &str, details: String) -> Self {
        Self {
            name: name.to_string(),
            success: false,
            message: message.to_string(),
            details: Some(details),
        }
    }
}

