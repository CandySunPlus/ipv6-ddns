use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, de};

use crate::{Settings, dns_name};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    #[default]
    Cloudflare,
    Aliyun,
}

impl Provider {
    pub fn name(self) -> &'static str {
        match self {
            Self::Cloudflare => "Cloudflare",
            Self::Aliyun => "阿里云 DNS",
        }
    }
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "provider", rename_all = "snake_case")]
pub enum Credentials {
    Cloudflare {
        token: String,
    },
    Aliyun {
        access_key_id: String,
        access_key_secret: String,
    },
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("provider", &self.provider())
            .finish_non_exhaustive()
    }
}

impl Credentials {
    pub fn provider(&self) -> Provider {
        match self {
            Self::Cloudflare { .. } => Provider::Cloudflare,
            Self::Aliyun { .. } => Provider::Aliyun,
        }
    }

    pub fn empty(provider: Provider) -> Self {
        match provider {
            Provider::Cloudflare => Self::Cloudflare {
                token: String::new(),
            },
            Provider::Aliyun => Self::Aliyun {
                access_key_id: String::new(),
                access_key_secret: String::new(),
            },
        }
    }

    fn resolve(self, previous: Option<&Self>) -> Result<Self, String> {
        fn field(mut value: String, old: Option<&str>, label: &str) -> Result<String, String> {
            let length = value.trim().len();
            if length == 0 {
                value.clear();
                value.push_str(old.unwrap_or(""));
            } else if length != value.len() {
                let start = value.len() - value.trim_start().len();
                value.drain(..start);
                value.truncate(length);
            }
            if value.is_empty() {
                return Err(format!("请输入 {label}。"));
            }
            if value.len() > 512 || !value.bytes().all(|byte| byte.is_ascii_graphic()) {
                return Err(format!("{label} 格式无效。"));
            }
            Ok(value)
        }
        match self {
            Self::Cloudflare { token } => {
                let old = match previous {
                    Some(Self::Cloudflare { token }) => Some(token.as_str()),
                    _ => None,
                };
                Ok(Self::Cloudflare {
                    token: field(token, old, "Cloudflare API Token")?,
                })
            }
            Self::Aliyun {
                access_key_id,
                access_key_secret,
            } => {
                let old = match previous {
                    Some(Self::Aliyun {
                        access_key_id,
                        access_key_secret,
                    }) => Some((access_key_id.as_str(), access_key_secret.as_str())),
                    _ => None,
                };
                let id = field(access_key_id, old.map(|old| old.0), "阿里云 AccessKey ID")?;
                let secret = field(
                    access_key_secret,
                    old.filter(|old| old.0 == id).map(|old| old.1),
                    "阿里云 AccessKey Secret",
                )?;
                Ok(Self::Aliyun {
                    access_key_id: id,
                    access_key_secret: secret,
                })
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Config {
    pub settings: Settings,
    pub credentials: Credentials,
}

impl Config {
    pub fn updated(
        mut settings: Settings,
        credentials: Credentials,
        previous: Option<&Self>,
    ) -> Result<Self, String> {
        settings.domain = dns_name(&settings.domain)?;
        if settings.provider != credentials.provider() {
            return Err("DNS 服务商与凭据类型不匹配。".into());
        }
        let credentials = credentials.resolve(previous.map(|old| &old.credentials))?;
        Ok(Self {
            settings,
            credentials,
        })
    }
}

impl<'de> Deserialize<'de> for Config {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Read existing installed Cloudflare configurations, but only write the new schema.
        #[derive(Deserialize)]
        struct Stored {
            settings: Settings,
            credentials: Option<Credentials>,
            token: Option<String>,
        }
        let stored = Stored::deserialize(deserializer)?;
        let credentials = match stored.credentials {
            Some(credentials) => credentials,
            None if stored.settings.provider == Provider::Cloudflare => Credentials::Cloudflare {
                token: stored
                    .token
                    .ok_or_else(|| de::Error::custom("缺少 Cloudflare 凭据。"))?,
            },
            None => return Err(de::Error::custom("缺少 DNS 服务商凭据。")),
        };
        Self::updated(stored.settings, credentials, None).map_err(de::Error::custom)
    }
}
