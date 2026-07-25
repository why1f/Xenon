//! Pure domain types and accounting rules shared by Panel and Agent.

use chrono::{Datelike, Duration, NaiveDate, Timelike, Utc};
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use thiserror::Error;

pub const BYTES_PER_GIB: u64 = 1_073_741_824;
pub const MAX_XRAY_CORE_VERSION: &str = "26.6.27";
pub const PANEL_AGENT_PROTOCOL_VERSION: &str = "0.1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BillingCycle {
    pub start: i64,
    pub end: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetPolicy {
    Never,
    Manual,
    Daily { seconds_utc: u32 },
    Monthly { day: u32, seconds_utc: u32 },
    IntervalDays { days: u32, anchor: i64 },
}

impl ResetPolicy {
    pub fn parse(value: &str, effective_start: i64) -> Result<Self, DomainError> {
        let value = value.trim().to_ascii_lowercase();
        if value.is_empty() || value == "never" {
            return Ok(Self::Never);
        }
        if value == "manual" {
            return Ok(Self::Manual);
        }
        if let Some(time) = value.strip_prefix("daily:") {
            return Ok(Self::Daily {
                seconds_utc: parse_utc_time(time)?,
            });
        }
        if let Some(value) = value.strip_prefix("monthly:") {
            let (day, time) = value
                .split_once('@')
                .ok_or(DomainError::InvalidResetPolicy)?;
            let day = day
                .parse::<u32>()
                .map_err(|_| DomainError::InvalidResetPolicy)?;
            if !(1..=31).contains(&day) {
                return Err(DomainError::InvalidResetPolicy);
            }
            return Ok(Self::Monthly {
                day,
                seconds_utc: parse_utc_time(time)?,
            });
        }
        if let Some(days) = value.strip_prefix("interval:") {
            let days = days
                .parse::<u32>()
                .map_err(|_| DomainError::InvalidResetPolicy)?;
            if !(1..=3650).contains(&days) {
                return Err(DomainError::InvalidResetPolicy);
            }
            return Ok(Self::IntervalDays {
                days,
                anchor: effective_start,
            });
        }
        Err(DomainError::InvalidResetPolicy)
    }

    pub fn from_stored(policy: &str, anchor: Option<i64>) -> Result<Self, DomainError> {
        match policy {
            "never" => Ok(Self::Never),
            "manual" => Ok(Self::Manual),
            "daily" => Ok(Self::Daily {
                seconds_utc: u32::try_from(anchor.ok_or(DomainError::InvalidResetPolicy)?)
                    .map_err(|_| DomainError::InvalidResetPolicy)?,
            }),
            "monthly" => {
                let encoded = u32::try_from(anchor.ok_or(DomainError::InvalidResetPolicy)?)
                    .map_err(|_| DomainError::InvalidResetPolicy)?;
                Ok(Self::Monthly {
                    day: encoded / 86_400,
                    seconds_utc: encoded % 86_400,
                })
            }
            value if value.starts_with("interval_days:") => {
                let days = value[14..]
                    .parse::<u32>()
                    .map_err(|_| DomainError::InvalidResetPolicy)?;
                Ok(Self::IntervalDays {
                    days,
                    anchor: anchor.ok_or(DomainError::InvalidResetPolicy)?,
                })
            }
            _ => Err(DomainError::InvalidResetPolicy),
        }
    }

    pub fn stored(self) -> (String, Option<i64>) {
        match self {
            Self::Never => ("never".into(), None),
            Self::Manual => ("manual".into(), None),
            Self::Daily { seconds_utc } => ("daily".into(), Some(i64::from(seconds_utc))),
            Self::Monthly { day, seconds_utc } => (
                "monthly".into(),
                Some(i64::from(day * 86_400 + seconds_utc)),
            ),
            Self::IntervalDays { days, anchor } => (format!("interval_days:{days}"), Some(anchor)),
        }
    }

    pub fn cycle_at(self, effective_start: i64, now: i64) -> Result<BillingCycle, DomainError> {
        match self {
            Self::Never | Self::Manual => Ok(BillingCycle {
                start: effective_start,
                end: None,
            }),
            Self::Daily { seconds_utc } => {
                if seconds_utc >= 86_400 {
                    return Err(DomainError::InvalidResetPolicy);
                }
                let day_start = now.div_euclid(86_400) * 86_400;
                let mut boundary = day_start + i64::from(seconds_utc);
                if boundary > now {
                    boundary -= 86_400;
                }
                Ok(BillingCycle {
                    start: boundary.max(effective_start),
                    end: Some(boundary.saturating_add(86_400)),
                })
            }
            Self::Monthly { day, seconds_utc } => {
                if !(1..=31).contains(&day) || seconds_utc >= 86_400 {
                    return Err(DomainError::InvalidResetPolicy);
                }
                let current = chrono::DateTime::<Utc>::from_timestamp(now, 0)
                    .ok_or(DomainError::InvalidResetPolicy)?;
                let current_boundary =
                    monthly_boundary(current.year(), current.month(), day, seconds_utc)?;
                let (start_year, start_month) = if current_boundary <= now {
                    (current.year(), current.month())
                } else {
                    previous_month(current.year(), current.month())
                };
                let start_boundary = monthly_boundary(start_year, start_month, day, seconds_utc)?;
                let (end_year, end_month) = next_month(start_year, start_month);
                let end_boundary = monthly_boundary(end_year, end_month, day, seconds_utc)?;
                Ok(BillingCycle {
                    start: start_boundary.max(effective_start),
                    end: Some(end_boundary),
                })
            }
            Self::IntervalDays { days, anchor } => {
                if days == 0 {
                    return Err(DomainError::InvalidResetPolicy);
                }
                let period = i64::from(days).saturating_mul(86_400);
                let elapsed = now.saturating_sub(anchor).max(0);
                let start = anchor.saturating_add(elapsed / period * period);
                Ok(BillingCycle {
                    start: start.max(effective_start),
                    end: Some(start.saturating_add(period)),
                })
            }
        }
    }
}

fn parse_utc_time(value: &str) -> Result<u32, DomainError> {
    let time = chrono::NaiveTime::from_str(value).map_err(|_| DomainError::InvalidResetPolicy)?;
    Ok(time.num_seconds_from_midnight())
}

fn monthly_boundary(
    year: i32,
    month: u32,
    requested_day: u32,
    seconds_utc: u32,
) -> Result<i64, DomainError> {
    let (next_year, next_month) = next_month(year, month);
    let next_first =
        NaiveDate::from_ymd_opt(next_year, next_month, 1).ok_or(DomainError::InvalidResetPolicy)?;
    let last_day = (next_first - Duration::days(1)).day();
    let date = NaiveDate::from_ymd_opt(year, month, requested_day.min(last_day))
        .ok_or(DomainError::InvalidResetPolicy)?;
    let hours = seconds_utc / 3600;
    let minutes = seconds_utc % 3600 / 60;
    let seconds = seconds_utc % 60;
    Ok(date
        .and_hms_opt(hours, minutes, seconds)
        .ok_or(DomainError::InvalidResetPolicy)?
        .and_utc()
        .timestamp())
}

fn previous_month(year: i32, month: u32) -> (i32, u32) {
    if month == 1 {
        (year - 1, 12)
    } else {
        (year, month - 1)
    }
}

fn next_month(year: i32, month: u32) -> (i32, u32) {
    if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BillingDirection {
    #[default]
    RxTx,
    TxOnly,
    RxOnly,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u32)]
pub enum TrafficMultiplier {
    #[default]
    One = 10_000,
    Two = 20_000,
}

impl TrafficMultiplier {
    pub const fn basis_points(self) -> u32 {
        self as u32
    }

    pub fn apply(self, bytes: u64) -> u64 {
        bytes.saturating_mul(self.basis_points() as u64) / 10_000
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrafficUsage {
    pub upload_bytes: u64,
    pub download_bytes: u64,
}

impl TrafficUsage {
    pub const fn new(upload_bytes: u64, download_bytes: u64) -> Self {
        Self {
            upload_bytes,
            download_bytes,
        }
    }

    pub fn total(self) -> u64 {
        self.upload_bytes.saturating_add(self.download_bytes)
    }

    pub fn saturating_add(self, rhs: Self) -> Self {
        Self {
            upload_bytes: self.upload_bytes.saturating_add(rhs.upload_bytes),
            download_bytes: self.download_bytes.saturating_add(rhs.download_bytes),
        }
    }

    pub fn apply_multiplier(self, multiplier: TrafficMultiplier) -> Self {
        Self {
            upload_bytes: multiplier.apply(self.upload_bytes),
            download_bytes: multiplier.apply(self.download_bytes),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterfaceCounters {
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

impl InterfaceCounters {
    pub const fn new(rx_bytes: u64, tx_bytes: u64) -> Self {
        Self { rx_bytes, tx_bytes }
    }

    pub fn checked_delta(self, previous: Self) -> Option<Self> {
        Some(Self {
            rx_bytes: self.rx_bytes.checked_sub(previous.rx_bytes)?,
            tx_bytes: self.tx_bytes.checked_sub(previous.tx_bytes)?,
        })
    }

    pub fn charged_usage(self, direction: BillingDirection) -> TrafficUsage {
        match direction {
            BillingDirection::RxTx => TrafficUsage::new(self.rx_bytes, self.tx_bytes),
            BillingDirection::TxOnly => TrafficUsage::new(0, self.tx_bytes),
            BillingDirection::RxOnly => TrafficUsage::new(self.rx_bytes, 0),
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DomainError {
    #[error("traffic limit must be greater than zero")]
    InvalidTrafficLimit,
    #[error("subscription must contain at least one node")]
    EmptyNodeSelection,
    #[error("subscription expiry must be after its start")]
    InvalidSubscriptionPeriod,
    #[error("invalid traffic reset policy")]
    InvalidResetPolicy,
}

pub fn validate_subscription(
    traffic_limit_bytes: Option<u64>,
    starts_at: i64,
    expires_at: Option<i64>,
    node_count: usize,
) -> Result<(), DomainError> {
    if traffic_limit_bytes == Some(0) {
        return Err(DomainError::InvalidTrafficLimit);
    }
    if node_count == 0 {
        return Err(DomainError::EmptyNodeSelection);
    }
    if expires_at.is_some_and(|end| end <= starts_at) {
        return Err(DomainError::InvalidSubscriptionPeriod);
    }
    Ok(())
}

pub fn interface_header_usage(
    counters: InterfaceCounters,
    direction: BillingDirection,
) -> TrafficUsage {
    counters.charged_usage(direction)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timestamp(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> i64 {
        chrono::TimeZone::with_ymd_and_hms(&Utc, year, month, day, hour, minute, 0)
            .single()
            .expect("valid test timestamp")
            .timestamp()
    }

    #[test]
    fn traffic_usage_saturates_and_applies_multiplier() {
        let usage = TrafficUsage::new(10, u64::MAX).saturating_add(TrafficUsage::new(5, 1));
        assert_eq!(usage.upload_bytes, 15);
        assert_eq!(usage.download_bytes, u64::MAX);
        assert_eq!(
            TrafficUsage::new(3, 7).apply_multiplier(TrafficMultiplier::Two),
            TrafficUsage::new(6, 14)
        );
    }

    #[test]
    fn interface_direction_maps_server_counters() {
        let counters = InterfaceCounters::new(20, 80);
        assert_eq!(
            counters.charged_usage(BillingDirection::RxTx),
            TrafficUsage::new(20, 80)
        );
        assert_eq!(
            counters.charged_usage(BillingDirection::TxOnly),
            TrafficUsage::new(0, 80)
        );
        assert_eq!(
            counters.charged_usage(BillingDirection::RxOnly),
            TrafficUsage::new(20, 0)
        );
    }

    #[test]
    fn counter_reset_is_not_a_negative_delta() {
        assert_eq!(
            InterfaceCounters::new(4, 2).checked_delta(InterfaceCounters::new(9, 2)),
            None
        );
    }

    #[test]
    fn subscription_validation_rejects_invalid_input() {
        assert_eq!(
            validate_subscription(Some(0), 0, Some(2), 1),
            Err(DomainError::InvalidTrafficLimit)
        );
        assert_eq!(
            validate_subscription(None, 0, Some(2), 0),
            Err(DomainError::EmptyNodeSelection)
        );
        assert_eq!(
            validate_subscription(None, 3, Some(2), 1),
            Err(DomainError::InvalidSubscriptionPeriod)
        );
        assert!(validate_subscription(Some(100), 0, Some(2), 1).is_ok());
    }

    #[test]
    fn reset_policies_calculate_daily_and_interval_cycles() {
        let effective_start = timestamp(2024, 1, 1, 0, 0);
        let daily = ResetPolicy::parse("daily:04:30", effective_start).expect("daily policy");
        assert_eq!(
            daily
                .cycle_at(effective_start, timestamp(2024, 1, 2, 3, 0))
                .expect("daily cycle"),
            BillingCycle {
                start: timestamp(2024, 1, 1, 4, 30),
                end: Some(timestamp(2024, 1, 2, 4, 30)),
            }
        );

        let interval = ResetPolicy::parse("interval:3", effective_start).expect("interval policy");
        assert_eq!(
            interval
                .cycle_at(effective_start, effective_start + 7 * 86_400)
                .expect("interval cycle"),
            BillingCycle {
                start: effective_start + 6 * 86_400,
                end: Some(effective_start + 9 * 86_400),
            }
        );
        let (stored, anchor) = interval.stored();
        assert_eq!(
            ResetPolicy::from_stored(&stored, anchor).expect("stored interval"),
            interval
        );
    }

    #[test]
    fn monthly_cycle_clamps_to_last_day_including_leap_years() {
        let effective_start = timestamp(2023, 1, 1, 0, 0);
        let policy =
            ResetPolicy::parse("monthly:31@00:00", effective_start).expect("monthly policy");
        assert_eq!(
            policy
                .cycle_at(effective_start, timestamp(2023, 2, 28, 12, 0))
                .expect("non-leap cycle"),
            BillingCycle {
                start: timestamp(2023, 2, 28, 0, 0),
                end: Some(timestamp(2023, 3, 31, 0, 0)),
            }
        );
        assert_eq!(
            policy
                .cycle_at(effective_start, timestamp(2024, 2, 29, 12, 0))
                .expect("leap cycle"),
            BillingCycle {
                start: timestamp(2024, 2, 29, 0, 0),
                end: Some(timestamp(2024, 3, 31, 0, 0)),
            }
        );
    }
}
