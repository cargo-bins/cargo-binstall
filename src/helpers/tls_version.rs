use clap::ArgEnum;
use reqwest::tls::Version;

#[derive(Debug, Copy, Clone, ArgEnum)]
pub enum TLSVersion {
    #[clap(name = "1.2")]
    Tls1_2,
    #[clap(name = "1.3")]
    Tls1_3,
}

impl From<TLSVersion> for Version {
    fn from(ver: TLSVersion) -> Self {
        match ver {
            TLSVersion::Tls1_2 => Version::TLS_1_2,
            TLSVersion::Tls1_3 => Version::TLS_1_3,
        }
    }
}
