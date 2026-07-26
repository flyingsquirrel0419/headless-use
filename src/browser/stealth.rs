//! Stealth mode: make `--headless=new` stop announcing itself.
//!
//! [Decision Log]
//! - 목적과 의도: Cloudflare Turnstile 같은 봇 감지가 뜨는 사이트를 헤드리스에서도
//!   통과하게 만든다. 헤드풀 Chrome은 통과하지만 무겁고, Xvfb 모드는 X 서버 + 렌더
//!   비용이 붙는다. 목표는 "가벼운 headless=new를 헤드리스처럼 보이지 않게" 하는 것.
//! - 기존 구현 및 제약 조건: headless=new는 `navigator.webdriver=true`,
//!   UA/Sec-CH-UA의 `HeadlessChrome`, SwiftShader WebGL 문자열, 브라우저 크롬이
//!   없어 `outerHeight == innerHeight`인 창 등으로 바로 식별된다. CDP만 쓰는 구조라
//!   외부 stealth 런타임(puppeteer-extra 등)을 끼울 자리가 없다.
//! - 검토한 주요 대안: (a) `--compat xvfb` — 실제로 헤드풀이라 지문은 깨끗하지만
//!   메모리/CPU가 배로 들고 Xvfb 설치가 전제라, 사용자가 피하려던 비용 그대로다.
//!   (b) JS 주입만 — `navigator.webdriver`는 가려도 UA와 Sec-CH-UA 헤더는 못 고친다.
//!   헤더는 JS보다 먼저 나가므로 감지에 그대로 걸린다. (c) 브라우저 플래그만 —
//!   `--disable-blink-features=AutomationControlled`와 `--user-agent`로 큰 신호는
//!   지우지만 WebGL 드라이버 문자열/화면 크기/plugins는 남는다.
//! - 선택한 방식: 세 층을 함께 쓴다. (1) 프로세스 전체에 적용되는 런치 플래그,
//!   (2) WebContents 단위로 UA와 Sec-CH-UA(client hints)를 함께 덮는
//!   `Emulation.setUserAgentOverride`, (3) 남은 JS 표면을 고치는 프리로드 스크립트.
//!   그리고 챌린지 위젯이 사는 크로스오리진 iframe(OOPIF)은 별도 타깃이라
//!   `Target.setAutoAttach`로 붙어 같은 처리를 적용한다.
//! - 다른 대안 대신 이 방식을 선택한 이유: 헤드리스의 가벼움을 유지하면서 감지 표면을
//!   실제로 덮는 유일한 조합이다. 각 층은 서로를 보완한다 — 플래그는 iframe까지
//!   자동 적용되고, UA 오버라이드는 헤더를 고치고, 스크립트는 JS 전용 표면을 고친다.
//! - 장점, 단점 및 영향: 장점은 추가 프로세스/의존성 0, 기본 동작 불변(플래그가 없으면
//!   코드 경로 자체가 꺼짐). 단점은 (i) 지문 위조는 원리상 군비경쟁이라 최신 감지에
//!   100% 보장이 없고, (ii) 스텔스에서는 `--hide-scrollbars`를 빼고 소프트웨어 WebGL을
//!   켜므로 스크롤바 폭과 GPU 초기화 비용이 기본 모드와 달라진다.

use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};

use crate::browser::{BrowserError, Page};
use crate::cdp::CdpClient;

/// Pre-load script that patches the JS-visible headless surface.
const STEALTH_JS: &str = include_str!("stealth.js");

/// Used when the browser refuses to report a version. Any recent major is
/// better than a UA that says `HeadlessChrome`, but a stale major is itself a
/// weak signal, so detection failure is logged.
const FALLBACK_FULL_VERSION: &str = "150.0.0.0";

/// A resolved stealth identity for one browser process.
///
/// Built once per launch: the user agent is derived from the *real* browser
/// version, so the UA string, the client-hint brand list and the JS engine
/// behaviour all agree. A UA claiming Chrome 120 on a Chrome 150 engine is a
/// mismatch detectors check for.
#[derive(Debug, Clone)]
pub struct StealthProfile {
    full_version: String,
    major_version: String,
    user_agent: String,
    accept_language: String,
}

impl StealthProfile {
    /// Build a profile by asking the browser binary for its version.
    pub async fn detect(exe: &Path) -> Self {
        let reported = tokio::process::Command::new(exe)
            .arg("--version")
            .output()
            .await
            .ok()
            .filter(|out| out.status.success())
            .and_then(|out| parse_version(&String::from_utf8_lossy(&out.stdout)));
        match reported {
            Some(v) => Self::from_full_version(&v),
            None => {
                tracing::warn!(
                    exe = %exe.display(),
                    "stealth: could not read browser version; using {FALLBACK_FULL_VERSION} for the UA"
                );
                Self::from_full_version(FALLBACK_FULL_VERSION)
            }
        }
    }

    /// Build a profile from a full version string like `150.0.7871.114`.
    pub fn from_full_version(full_version: &str) -> Self {
        let major_version = full_version
            .split('.')
            .next()
            .unwrap_or("150")
            .trim()
            .to_string();
        // Chrome froze the minor/build/patch fields of the UA at `.0.0.0` (the
        // "reduced UA"), so this is the exact shape a real Chrome sends.
        let user_agent = format!(
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) \
             Chrome/{major_version}.0.0.0 Safari/537.36"
        );
        Self {
            full_version: full_version.to_string(),
            major_version,
            user_agent,
            accept_language: "en-US,en;q=0.9".to_string(),
        }
    }

    /// The user agent this profile presents.
    pub fn user_agent(&self) -> &str {
        &self.user_agent
    }

    /// Chrome flags that suppress process-wide automation signals.
    ///
    /// These matter more than any JS patch because they apply to every frame in
    /// the process — including the cross-origin iframe a CAPTCHA widget runs in,
    /// which a page-scoped CDP command cannot reach before its first paint.
    pub fn launch_args(&self) -> Vec<String> {
        vec![
            // Removes `navigator.webdriver` at the source. Patching it from JS
            // leaves a non-native getter behind; this leaves nothing.
            "--disable-blink-features=AutomationControlled".to_string(),
            // Drops `HeadlessChrome/...` from the UA string for every request,
            // including ones we never attach to.
            format!("--user-agent={}", self.user_agent),
            // Keep the locale consistent with the Accept-Language we send.
            "--lang=en-US".to_string(),
            // Chrome 128+ refuses the software GL fallback without this. A
            // browser with no WebGL at all is more suspicious than one with a
            // software renderer, and the renderer string is spoofed in JS.
            "--enable-unsafe-swiftshader".to_string(),
        ]
    }

    /// Params for `Emulation.setUserAgentOverride`.
    ///
    /// This is the only way to fix `Sec-CH-UA`: user-agent client hints are
    /// generated from the browser's own brand list, which says `HeadlessChrome`
    /// no matter what `--user-agent` is set to. Applied on a page session it
    /// takes effect for the whole WebContents, so subframes send it too.
    pub fn user_agent_override_params(&self) -> Value {
        let major = &self.major_version;
        let full = &self.full_version;
        json!({
            "userAgent": self.user_agent,
            "acceptLanguage": self.accept_language,
            "platform": "Linux x86_64",
            "userAgentMetadata": {
                // The first entry is Chrome's GREASE brand: intentional junk
                // that real Chrome always includes, so omitting it stands out.
                "brands": [
                    { "brand": "Not)A;Brand", "version": "8" },
                    { "brand": "Chromium", "version": major },
                    { "brand": "Google Chrome", "version": major },
                ],
                "fullVersionList": [
                    { "brand": "Not)A;Brand", "version": "8.0.0.0" },
                    { "brand": "Chromium", "version": full },
                    { "brand": "Google Chrome", "version": full },
                ],
                "fullVersion": full,
                "platform": "Linux",
                // Chrome on Linux reports an empty platform version.
                "platformVersion": "",
                "architecture": "x86",
                "model": "",
                "mobile": false,
                "bitness": "64",
                "wow64": false,
            }
        })
    }

    /// Apply the profile to a page: UA/client-hint override, pre-load script,
    /// and auto-attach so cross-origin subframes get the same treatment.
    ///
    /// Errors are returned rather than swallowed: a session asked for stealth
    /// and silently getting a bare headless browser is the failure mode that
    /// wastes the most time.
    pub async fn apply_to_page(&self, page: &Page) -> Result<(), BrowserError> {
        page.call::<Value>(
            "Emulation.setUserAgentOverride",
            Some(self.user_agent_override_params()),
            Duration::from_secs(10),
        )
        .await?;
        page.call::<Value>(
            "Page.addScriptToEvaluateOnNewDocument",
            Some(json!({ "source": STEALTH_JS })),
            Duration::from_secs(10),
        )
        .await?;
        // Cross-origin iframes are separate CDP targets: the page session's
        // pre-load script never reaches them, and a challenge widget does its
        // fingerprinting from inside exactly such an iframe. Auto-attach makes
        // every child target visible; `watch_child_targets` patches each one.
        //
        // The watcher is registered *before* auto-attach is turned on, and
        // `watch_child_targets` awaits its own subscription before returning: a
        // frame that attaches while nobody is listening stays paused forever and
        // hangs the page.
        self.watch_child_targets(page.cdp().clone()).await;
        page.call::<Value>(
            "Target.setAutoAttach",
            Some(json!({
                "autoAttach": true,
                // Hold the child until we have injected, then resume it. Without
                // this the iframe's own scripts can win the race.
                "waitForDebuggerOnStart": true,
                "flatten": true,
            })),
            Duration::from_secs(10),
        )
        .await?;
        Ok(())
    }

    /// Spawn a task that patches every target that auto-attaches, then resumes
    /// it. The task ends when the CDP connection closes.
    ///
    /// Invariant: a target paused by `waitForDebuggerOnStart` MUST be resumed on
    /// every path, including injection failure — otherwise the iframe hangs and
    /// the page never finishes loading.
    async fn watch_child_targets(&self, client: CdpClient) {
        let profile = self.clone();
        let mut events = client.subscribe_events_async().await;
        tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                if event.method != "Target.attachedToTarget" {
                    continue;
                }
                let Some(session_id) = event
                    .params
                    .get("sessionId")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                else {
                    continue;
                };
                let target_type = event
                    .params
                    .get("targetInfo")
                    .and_then(|t| t.get("type"))
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string();
                let waiting = event
                    .params
                    .get("waitingForDebugger")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let client = client.clone();
                let profile = profile.clone();
                tokio::spawn(async move {
                    // Only document-bearing targets have a Page domain; workers
                    // and other target types just need resuming.
                    if matches!(target_type.as_str(), "iframe" | "page" | "webview") {
                        profile.patch_child_session(&client, &session_id).await;
                    }
                    if waiting {
                        let _ = client
                            .call_session::<Value>(
                                "Runtime.runIfWaitingForDebugger",
                                None,
                                &session_id,
                                Duration::from_secs(5),
                            )
                            .await;
                    }
                });
            }
        });
    }

    /// Inject into one auto-attached child session. Best-effort: a frame that
    /// dies mid-attach must not stop the rest.
    async fn patch_child_session(&self, client: &CdpClient, session_id: &str) {
        let calls: [(&str, Value); 3] = [
            (
                "Page.addScriptToEvaluateOnNewDocument",
                json!({ "source": STEALTH_JS }),
            ),
            (
                "Emulation.setUserAgentOverride",
                self.user_agent_override_params(),
            ),
            // Nested cross-origin frames are targets of this target.
            (
                "Target.setAutoAttach",
                json!({ "autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true }),
            ),
        ];
        for (method, params) in calls {
            if let Err(e) = client
                .call_session::<Value>(method, Some(params), session_id, Duration::from_secs(5))
                .await
            {
                tracing::debug!(session = %session_id, method, error = %e, "stealth: child patch failed");
            }
        }
    }
}

/// Pull `150.0.7871.114` out of `Google Chrome 150.0.7871.114` /
/// `Chromium 141.0.7390.65 snap`.
fn parse_version(output: &str) -> Option<String> {
    output
        .split_whitespace()
        .find(|tok| {
            tok.split('.').count() >= 2
                && tok
                    .split('.')
                    .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
        })
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_chrome_and_chromium_version_banners() {
        assert_eq!(
            parse_version("Google Chrome 150.0.7871.114 \n").as_deref(),
            Some("150.0.7871.114")
        );
        assert_eq!(
            parse_version("Chromium 141.0.7390.65 snap\n").as_deref(),
            Some("141.0.7390.65")
        );
        assert_eq!(parse_version("no version here").as_deref(), None);
    }

    #[test]
    fn user_agent_has_no_headless_marker() {
        let p = StealthProfile::from_full_version("150.0.7871.114");
        assert!(!p.user_agent().contains("Headless"));
        assert!(p.user_agent().contains("Chrome/150.0.0.0"));
        assert!(p.user_agent().contains("X11; Linux x86_64"));
    }

    #[test]
    fn client_hint_brands_match_the_real_major() {
        let p = StealthProfile::from_full_version("141.0.7390.65");
        let params = p.user_agent_override_params();
        let brands = params["userAgentMetadata"]["brands"].as_array().unwrap();
        let rendered = serde_json::to_string(brands).unwrap();
        assert!(rendered.contains("\"Google Chrome\""));
        assert!(rendered.contains("\"141\""));
        assert!(!rendered.contains("Headless"));
        assert_eq!(
            params["userAgentMetadata"]["fullVersion"].as_str(),
            Some("141.0.7390.65")
        );
    }

    #[test]
    fn launch_args_carry_the_ua_and_kill_automation_hint() {
        let p = StealthProfile::from_full_version("150.0.7871.114");
        let args = p.launch_args();
        assert!(args
            .iter()
            .any(|a| a == "--disable-blink-features=AutomationControlled"));
        assert!(args
            .iter()
            .any(|a| a.starts_with("--user-agent=") && !a.contains("Headless")));
    }

    #[test]
    fn preload_script_is_self_contained_and_guarded() {
        // The script runs in every frame of every document; a syntax-level
        // regression here silently disables stealth, so pin its shape.
        assert!(STEALTH_JS.contains("Function.prototype.toString"));
        assert!(STEALTH_JS.contains("navigator.webdriver !== false"));
        assert!(STEALTH_JS.starts_with("//"));
    }
}
