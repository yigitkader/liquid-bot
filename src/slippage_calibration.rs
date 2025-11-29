use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Slippage measurement from a real transaction
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlippageMeasurement {
    /// Transaction signature
    pub signature: String,
    /// Trade size in USD
    pub trade_size_usd: f64,
    /// Estimated slippage (bps) - what we predicted
    pub estimated_slippage_bps: u16,
    /// Actual slippage (bps) - measured from transaction
    pub actual_slippage_bps: Option<u16>,
    /// Timestamp of measurement
    pub timestamp: i64,
    /// Trade size category (small/medium/large)
    pub size_category: String,
}

/// Calibrated multipliers based on real measurements
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalibratedMultipliers {
    /// Small trade multiplier (<$10k)
    pub small: Option<f64>,
    /// Medium trade multiplier ($10k-$100k)
    pub medium: Option<f64>,
    /// Large trade multiplier (>$100k)
    pub large: Option<f64>,
    /// Number of measurements used for calibration
    pub measurement_count: usize,
    /// Last calibration timestamp
    pub last_calibrated: Option<i64>,
}

/// Slippage calibration data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlippageCalibrationData {
    /// All measurements
    pub measurements: Vec<SlippageMeasurement>,
    /// Calibrated multipliers
    pub calibrated: CalibratedMultipliers,
}

impl Default for SlippageCalibrationData {
    fn default() -> Self {
        SlippageCalibrationData {
            measurements: Vec::new(),
            calibrated: CalibratedMultipliers {
                small: None,
                medium: None,
                large: None,
                measurement_count: 0,
                last_calibrated: None,
            },
        }
    }
}

/// Manages slippage calibration based on real transaction data
pub struct SlippageCalibrator {
    data: Arc<RwLock<SlippageCalibrationData>>,
    file_path: String,
    small_threshold: f64,
    large_threshold: f64,
    max_slippage_bps: u16,
    min_measurements_per_category: usize,
}

impl SlippageCalibrator {
    /// Creates a new slippage calibrator
    pub fn new(
        file_path: String,
        small_threshold: f64,
        large_threshold: f64,
        max_slippage_bps: u16,
        min_measurements_per_category: usize,
    ) -> Result<Self> {
        let data = if Path::new(&file_path).exists() {
            let content = fs::read_to_string(&file_path)
                .context("Failed to read slippage calibration file")?;
            serde_json::from_str(&content)
                .context("Failed to parse slippage calibration file")?
        } else {
            SlippageCalibrationData::default()
        };

        Ok(SlippageCalibrator {
            data: Arc::new(RwLock::new(data)),
            file_path,
            small_threshold,
            large_threshold,
            max_slippage_bps,
            min_measurements_per_category,
        })
    }

    /// Records a slippage measurement
    pub async fn record_measurement(
        &self,
        signature: String,
        trade_size_usd: f64,
        estimated_slippage_bps: u16,
        actual_slippage_bps: Option<u16>,
    ) -> Result<()> {
        let size_category = if trade_size_usd < self.small_threshold {
            "small"
        } else if trade_size_usd > self.large_threshold {
            "large"
        } else {
            "medium"
        };

        let measurement = SlippageMeasurement {
            signature,
            trade_size_usd,
            estimated_slippage_bps,
            actual_slippage_bps,
            timestamp: chrono::Utc::now().timestamp(),
            size_category: size_category.to_string(),
        };

        let mut data = self.data.write().await;
        data.measurements.push(measurement);

        // Keep only last 1000 measurements to prevent file from growing too large
        if data.measurements.len() > 1000 {
            data.measurements.remove(0);
        }

        self.save().await?;
        Ok(())
    }

    /// Calculates calibrated multipliers from measurements
    pub async fn calibrate(&self) -> Result<CalibratedMultipliers> {
        let data = self.data.read().await;

        // Group measurements by size category
        let mut small_measurements = Vec::new();
        let mut medium_measurements = Vec::new();
        let mut large_measurements = Vec::new();

        for m in &data.measurements {
            if let Some(actual) = m.actual_slippage_bps {
                match m.size_category.as_str() {
                    "small" => small_measurements.push((m.estimated_slippage_bps, actual)),
                    "medium" => medium_measurements.push((m.estimated_slippage_bps, actual)),
                    "large" => large_measurements.push((m.estimated_slippage_bps, actual)),
                    _ => {}
                }
            }
        }

        // Calculate multipliers for each category
        let small_multiplier = if small_measurements.len() >= self.min_measurements_per_category {
            Self::calculate_multiplier(&small_measurements, self.max_slippage_bps)
        } else {
            None
        };

        let medium_multiplier = if medium_measurements.len() >= self.min_measurements_per_category {
            Self::calculate_multiplier(&medium_measurements, self.max_slippage_bps)
        } else {
            None
        };

        let large_multiplier = if large_measurements.len() >= self.min_measurements_per_category {
            Self::calculate_multiplier(&large_measurements, self.max_slippage_bps)
        } else {
            None
        };

        let total_measurements = small_measurements.len()
            + medium_measurements.len()
            + large_measurements.len();

        Ok(CalibratedMultipliers {
            small: small_multiplier,
            medium: medium_multiplier,
            large: large_multiplier,
            measurement_count: total_measurements,
            last_calibrated: Some(chrono::Utc::now().timestamp()),
        })
    }

    /// Calculates multiplier from measurements
    /// Returns the ratio of actual slippage to estimated slippage
    fn calculate_multiplier(measurements: &[(u16, u16)], max_slippage_bps: u16) -> Option<f64> {
        if measurements.is_empty() {
            return None;
        }

        // Calculate average ratio of actual to estimated slippage
        let mut ratios = Vec::new();
        for (estimated, actual) in measurements {
            if *estimated > 0 {
                let ratio = *actual as f64 / *estimated as f64;
                ratios.push(ratio);
            }
        }

        if ratios.is_empty() {
            return None;
        }

        // Use median to avoid outliers
        ratios.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let median_ratio = if ratios.len() % 2 == 0 {
            (ratios[ratios.len() / 2 - 1] + ratios[ratios.len() / 2]) / 2.0
        } else {
            ratios[ratios.len() / 2]
        };

        // Clamp to reasonable range (0.1x to 3.0x)
        Some(median_ratio.clamp(0.1, 3.0))
    }

    /// Gets calibrated multiplier for a trade size
    pub async fn get_calibrated_multiplier(&self, trade_size_usd: f64) -> Option<f64> {
        let data = self.data.read().await;

        if trade_size_usd < self.small_threshold {
            data.calibrated.small
        } else if trade_size_usd > self.large_threshold {
            data.calibrated.large
        } else {
            data.calibrated.medium
        }
    }

    /// Updates calibrated multipliers
    pub async fn update_calibrated(&self) -> Result<()> {
        let calibrated = self.calibrate().await?;
        let mut data = self.data.write().await;
        data.calibrated = calibrated;
        self.save().await?;

        log::info!(
            "Slippage calibration updated: small={:?}, medium={:?}, large={:?}, measurements={}",
            data.calibrated.small,
            data.calibrated.medium,
            data.calibrated.large,
            data.calibrated.measurement_count
        );

        Ok(())
    }

    /// Saves calibration data to file
    async fn save(&self) -> Result<()> {
        let data = self.data.read().await;
        let content = serde_json::to_string_pretty(&*data)
            .context("Failed to serialize slippage calibration data")?;
        fs::write(&self.file_path, content)
            .context("Failed to write slippage calibration file")?;
        Ok(())
    }

    /// Gets calibration status
    pub async fn get_status(&self) -> CalibratedMultipliers {
        let data = self.data.read().await;
        data.calibrated.clone()
    }
}

/// Calculates actual slippage from transaction data
/// This is a placeholder - actual implementation would parse transaction logs
/// or query Solscan API to get real slippage
pub async fn calculate_actual_slippage(
    _signature: &str,
    _trade_size_usd: f64,
    _estimated_slippage_bps: u16,
) -> Result<Option<u16>> {
    // TODO: Implement actual slippage calculation from transaction
    // This could:
    // 1. Parse transaction logs to find swap amounts
    // 2. Query Solscan API for transaction details
    // 3. Compare expected vs actual output amounts
    // 4. Calculate slippage: (expected - actual) / expected * 10000
    
    // For now, return None to indicate measurement not available
    Ok(None)
}

