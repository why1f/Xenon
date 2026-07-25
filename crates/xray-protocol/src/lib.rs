//! Minimal protobuf subset copied from Xray-core v26.6.27 API contracts.

pub mod xray {
    pub mod app {
        pub mod proxyman {
            pub mod command {
                tonic::include_proto!("xray.app.proxyman.command");
            }
        }
        pub mod stats {
            pub mod command {
                tonic::include_proto!("xray.app.stats.command");
            }
        }
    }
    pub mod common {
        pub mod protocol {
            tonic::include_proto!("xray.common.protocol");
        }
        pub mod serial {
            tonic::include_proto!("xray.common.serial");
        }
    }
    pub mod proxy {
        pub mod vless {
            tonic::include_proto!("xray.proxy.vless");
        }
    }
}

pub const ADD_USER_OPERATION_TYPE: &str = "xray.app.proxyman.command.AddUserOperation";
pub const REMOVE_USER_OPERATION_TYPE: &str = "xray.app.proxyman.command.RemoveUserOperation";
pub const VLESS_ACCOUNT_TYPE: &str = "xray.proxy.vless.Account";
