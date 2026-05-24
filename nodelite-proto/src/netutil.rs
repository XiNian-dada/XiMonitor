use url::Url;

/// 判定 host 字段是否表示本机:`localhost` 或回环 IP。
pub fn host_is_local(host: Option<&str>) -> bool {
    let Some(host) = host else {
        return false;
    };
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }

    host.parse::<std::net::IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

/// 判断一个 URL 是否使用了指定的不安全 scheme,且目标并非本地回环地址。
pub fn uses_insecure_remote_url(url: &str, insecure_scheme: &str) -> bool {
    let Ok(parsed) = Url::parse(url) else {
        return false;
    };
    if parsed.scheme() != insecure_scheme {
        return false;
    }

    !host_is_local(parsed.host_str())
}

#[cfg(test)]
mod tests {
    use super::{host_is_local, uses_insecure_remote_url};

    #[test]
    fn host_is_local_matches_loopback_hosts() {
        assert!(host_is_local(Some("localhost")));
        assert!(host_is_local(Some("LOCALHOST")));
        assert!(host_is_local(Some("127.0.0.1")));
        assert!(host_is_local(Some("::1")));
        assert!(!host_is_local(Some("example.com")));
        assert!(!host_is_local(None));
    }

    #[test]
    fn uses_insecure_remote_url_only_flags_non_local_hosts() {
        assert!(uses_insecure_remote_url(
            "http://monitor.example.com",
            "http"
        ));
        assert!(uses_insecure_remote_url("ws://203.0.113.10/ws", "ws"));
        assert!(!uses_insecure_remote_url(
            "https://monitor.example.com",
            "http"
        ));
        assert!(!uses_insecure_remote_url("ws://localhost:8080/ws", "ws"));
        assert!(!uses_insecure_remote_url("ws://127.0.0.1:8080/ws", "ws"));
        assert!(!uses_insecure_remote_url("not a valid url", "http"));
    }
}
