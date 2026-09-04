//! Shared, role-neutral FarHelm primitives.

use serde::{Deserialize, Serialize};

pub const PRODUCT_NAME: &str = "FarHelm";
pub const PRODUCT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildInfo {
    pub product: &'static str,
    pub version: &'static str,
}

impl BuildInfo {
    #[must_use]
    pub const fn current() -> Self {
        Self {
            product: PRODUCT_NAME,
            version: PRODUCT_VERSION,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_info_uses_workspace_identity() {
        let info = BuildInfo::current();
        assert_eq!(info.product, "FarHelm");
        assert_eq!(info.version, "0.4.1");
    }
}
