// Validation macro'ları - tekrarlayan pattern'leri basitleştirir

#[macro_export]
macro_rules! validate {
    ($name:expr, $condition:expr, $success_msg:expr, $fail_msg:expr) => {
        if $condition {
            $crate::bin::validation::result::TestResult::success($name, $success_msg)
        } else {
            $crate::bin::validation::result::TestResult::failure($name, $fail_msg)
        }
    };
}

#[macro_export]
macro_rules! validate_result {
    ($name:expr, $result:expr, $success_msg:expr) => {
        match $result {
            Ok(_) => $crate::bin::validation::result::TestResult::success($name, $success_msg),
            Err(e) => $crate::bin::validation::result::TestResult::failure($name, &format!("Failed: {}", e)),
        }
    };
}

#[macro_export]
macro_rules! validate_option {
    ($name:expr, $opt:expr, $success_msg:expr, $fail_msg:expr) => {
        match $opt {
            Some(_) => $crate::bin::validation::result::TestResult::success($name, $success_msg),
            None => $crate::bin::validation::result::TestResult::failure($name, $fail_msg),
        }
    };
}

