// ValidationBuilder - fluent API ile validation'ları kolayca oluşturmayı sağlar

use super::result::TestResult;

pub struct ValidationBuilder {
    results: Vec<TestResult>,
}

impl ValidationBuilder {
    pub fn new() -> Self {
        Self {
            results: Vec::new(),
        }
    }

    /// Basit boolean check
    pub fn check<F>(mut self, name: &str, check: F) -> Self
    where
        F: FnOnce() -> bool,
    {
        let result = if check() {
            TestResult::success(name, "Valid")
        } else {
            TestResult::failure(name, "Invalid")
        };
        self.results.push(result);
        self
    }

    /// Boolean check with custom messages
    pub fn check_with_msg<F>(
        mut self,
        name: &str,
        check: F,
        success_msg: &str,
        fail_msg: &str,
    ) -> Self
    where
        F: FnOnce() -> bool,
    {
        let result = if check() {
            TestResult::success(name, success_msg)
        } else {
            TestResult::failure(name, fail_msg)
        };
        self.results.push(result);
        self
    }

    /// Boolean check with success message and format function for failure
    pub fn check_with_format<F>(
        mut self,
        name: &str,
        check: F,
        success_msg: &str,
        fail_format: impl FnOnce() -> String,
    ) -> Self
    where
        F: FnOnce() -> bool,
    {
        let result = if check() {
            TestResult::success(name, success_msg)
        } else {
            TestResult::failure(name, &fail_format())
        };
        self.results.push(result);
        self
    }

    /// Result<T> check - async operation sonucunu kontrol eder
    pub fn check_result<T, E>(
        mut self,
        name: &str,
        result: Result<T, E>,
        success_msg: &str,
    ) -> Self
    where
        E: std::fmt::Display,
    {
        let result = match result {
            Ok(_) => TestResult::success(name, success_msg),
            Err(e) => TestResult::failure(name, &format!("Failed: {}", e)),
        };
        self.results.push(result);
        self
    }

    /// Result<T> check with custom error message
    pub fn check_result_with_msg<T, E>(
        mut self,
        name: &str,
        result: Result<T, E>,
        success_msg: &str,
        fail_format: impl FnOnce(E) -> String,
    ) -> Self
    where
        E: std::fmt::Display,
    {
        let result = match result {
            Ok(_) => TestResult::success(name, success_msg),
            Err(e) => TestResult::failure(name, &fail_format(e)),
        };
        self.results.push(result);
        self
    }

    /// Option<T> check
    pub fn check_option<T>(
        mut self,
        name: &str,
        opt: Option<T>,
        success_msg: &str,
        fail_msg: &str,
    ) -> Self {
        let result = match opt {
            Some(_) => TestResult::success(name, success_msg),
            None => TestResult::failure(name, fail_msg),
        };
        self.results.push(result);
        self
    }

    /// Result with details
    pub fn check_result_with_details<T, E>(
        mut self,
        name: &str,
        result: Result<T, E>,
        success_msg: &str,
        details: impl FnOnce(&T) -> String,
    ) -> Self
    where
        E: std::fmt::Display,
    {
        let result = match result {
            Ok(value) => TestResult::success_with_details(name, success_msg, details(&value)),
            Err(e) => TestResult::failure(name, &format!("Failed: {}", e)),
        };
        self.results.push(result);
        self
    }

    /// Manual result ekleme
    pub fn add_result(mut self, result: TestResult) -> Self {
        self.results.push(result);
        self
    }

    /// Build - tüm result'ları döndür
    pub fn build(self) -> Vec<TestResult> {
        self.results
    }
}

impl Default for ValidationBuilder {
    fn default() -> Self {
        Self::new()
    }
}

