//! Re-resolving an observed `@eN` reference to live viewport coordinates.
//!
//! Split out of `session/mod.rs`, which had accumulated JS string generation,
//! CSS selector mapping and coordinate math alongside the session's own
//! lifecycle. None of that needs to sit next to `Session::start`.

use crate::browser::BrowserError;
use crate::session::Session;

impl Session {
    /// Re-query an observed element's current viewport-relative bounding box.
    ///
    /// `obs_scroll_x`/`obs_scroll_y` are the page scroll offsets recorded when
    /// the observation was taken.
    pub(super) async fn resolve_element_rect(
        &self,
        el: &crate::observe::ElementRef,
        obs_scroll_x: i64,
        obs_scroll_y: i64,
    ) -> Result<(f64, f64, f64, f64), BrowserError> {
        // Re-resolve the element's current bounds. We inject the search text via
        // JSON.stringify so any character (Korean, quotes, backslashes) is safe
        // and cannot break the JS string literal.
        // [Decision Log]
        // - 목적과 의도: 한글/특수문자가 포함된 요소 이름으로 안전하게 클릭 좌표 재해결.
        // - 기존 구현 및 제약 조건: 이름을 JS 단일 따옴표 문자열에 직접 삽입하여
        //   따옴표/백슬래시 외의 특수문자에서 JS 파싱 에러 발생.
        // - 검토한 주요 대안: 수동 이스케이프 확장, base64 인코딩.
        // - 선택한 방식: serde_json::to_string으로 JSON 문자열 생성 후 JS에 주입.
        // - 다른 대안 대신 이 방식을 선택한 이유: 모든 Unicode 문자에 안전, 간결함.
        // - 장점, 단점 및 영향: 한글/이모지/따옴표 모두 안전; 약간의 직렬화 오버헤드.
        // Document-space centre of the element as it was observed. Viewport
        // coordinates alone are useless for disambiguation because the page may
        // have scrolled between observe and click.
        let target_cx = el.x as f64 + el.width as f64 / 2.0 + obs_scroll_x as f64;
        let target_cy = el.y as f64 + el.height as f64 / 2.0 + obs_scroll_y as f64;

        let expr = if !el.selector_hint.is_empty() && el.selector_hint.starts_with('#') {
            // `getElementById` takes the raw id, so there is no escaping problem
            // to get wrong. The previous code ran the id through a *filter* that
            // deleted every character outside `[A-Za-z0-9_-]` — so `user.name`
            // silently became `username` and resolved a different element (or
            // none). Never rewrite an identifier to make it parse.
            let id_json = serde_json::to_string(el.selector_hint.trim_start_matches('#'))
                .unwrap_or_else(|_| "\"\"".into());
            format!(
                "(()=>{{const e=document.getElementById({id_json});if(!e)return null;e.scrollIntoView({{block:'center'}});const r=e.getBoundingClientRect();return JSON.stringify({{x:r.x,y:r.y,w:r.width,h:r.height}});}})()"
            )
        } else {
            let name_json = serde_json::to_string(&el.name).unwrap_or_else(|_| "\"\"".into());
            let tag = tag_for_query(&el.tag_name);
            // [Decision Log] — querySelector 인자 안전 주입
            // - 목적과 의도: tag_for_query가 `a[href], [role='link']`처럼 작은따옴표를
            //   포함하는 선택자를 반환한다. 이전에는 이 선택자를 JS 문자열 리터럴
            //   안의 작은따옴표 영역(`'{tag}'`)에 직접 끼워 넣었기 때문에 안쪽
            //   작은따옴표가 문자열을 일찍 닫아버려 `SyntaxError: missing ) after
            //   argument list`가 발생했다. (구글 결과 페이지의 링크 클릭이 전부
            //   실패한 원인.)
            // - 검토한 주요 대안: 선택자에서 작은따옴표를 제거, 큰따옴표로 래핑.
            // - 선택한 방식: name과 동일하게 tag도 JSON 문자열로 인코딩하여 JS
            //   변수에 할당한 뒤 querySelectorAll에 전달.
            // - 장점: 임의의 CSS 선택자 문자에 안전, name과 일관된 패턴.
            let tag_json = serde_json::to_string(&tag).unwrap_or_else(|_| "\"*\"".into());
            // [Decision Log] — 이름 중복 요소의 좌표 기반 판별
            // - 목적과 의도: 같은 이름을 가진 요소가 여러 개일 때 observe가 가리킨
            //   그 요소를 다시 찾는다.
            // - 기존 구현 및 제약 조건: `els.find(...)`로 텍스트가 일치하는 **첫**
            //   요소를 골랐다. "Delete" 버튼이 두 개인 목록에서는 항상 첫 번째가
            //   클릭됐고, 접근 이름이 빈 요소(`t === ""`)는 텍스트 없는 아무
            //   아이콘 버튼에나 일치했다. observe가 이미 알고 있는 좌표를 버린 것이
            //   원인이다.
            // - 검토한 주요 대안: 요소마다 고유 selector 생성, DOM 경로 저장,
            //   CDP objectId 보관(내비게이션에서 무효화되어 탈락).
            // - 선택한 방식: 이름이 일치하는 후보를 모두 모은 뒤, observe 당시의
            //   문서 좌표 중심과 가장 가까운 후보를 고른다. 이름이 비었으면 이름
            //   일치를 건너뛰고 좌표만으로 고른다.
            // - 다른 대안 대신 이 방식을 선택한 이유: 추가 상태 저장 없이 관찰
            //   시점에 이미 기록된 정보(x/y/width/height + scroll)만으로 판별된다.
            // - 장점, 단점 및 영향: 중복 이름과 익명 요소가 정확해진다. 관찰 이후
            //   레이아웃이 크게 바뀌면 가장 가까운 후보가 오답일 수 있으나, 항상
            //   첫 번째를 고르던 이전 동작보다 나쁠 수 없다.
            format!(
                "(()=>{{const sel={tag_json};const t={name_json};const tcx={target_cx};const tcy={target_cy};\
const els=[...document.querySelectorAll(sel)];\
const named=els.filter(x=>((x.innerText||x.textContent||'').trim()===t)||(x.getAttribute('aria-label')===t)||(x.getAttribute('placeholder')===t));\
const pool=named.length?named:(t===''?els:[]);\
if(!pool.length)return null;\
let best=null,bd=Infinity;\
for(const x of pool){{const r=x.getBoundingClientRect();\
const cx=r.x+r.width/2+window.scrollX,cy=r.y+r.height/2+window.scrollY;\
const d=(cx-tcx)*(cx-tcx)+(cy-tcy)*(cy-tcy);\
if(d<bd){{bd=d;best=x;}}}}\
if(!best)return null;\
best.scrollIntoView({{block:'center'}});\
const r=best.getBoundingClientRect();\
return JSON.stringify({{x:r.x,y:r.y,w:r.width,h:r.height,candidates:pool.length}});}})()"
            )
        };
        let result = self.page.evaluate_sync(&expr).await?;
        let value_str = result.value().and_then(|v| v.as_str()).map(String::from);
        let Some(s) = value_str else {
            return Err(BrowserError::ElementNotFound(format!(
                "@e{} could not be re-resolved (page changed; run observe again)",
                el.ref_id
            )));
        };
        if s == "null" || s.is_empty() {
            return Err(BrowserError::ElementNotFound(format!(
                "@e{} no longer exists (stale; run observe again)",
                el.ref_id
            )));
        }
        let v: serde_json::Value = serde_json::from_str(&s)
            .map_err(|e| BrowserError::Decode(format!("element bounds: {e}")))?;
        let x = v.get("x").and_then(|x| x.as_f64()).unwrap_or(0.0);
        let y = v.get("y").and_then(|y| y.as_f64()).unwrap_or(0.0);
        let w = v.get("w").and_then(|w| w.as_f64()).unwrap_or(0.0);
        let h = v.get("h").and_then(|h| h.as_f64()).unwrap_or(0.0);
        if w <= 0.0 || h <= 0.0 {
            return Err(BrowserError::ElementNotInteractable(format!(
                "@e{} has zero size (hidden or collapsed)",
                el.ref_id
            )));
        }
        Ok((x, y, w, h))
    }
}

/// Map a tag name to a query selector for name-based re-resolution.
///
/// For tags without a dedicated mapping (div, span, li, ...) the element's
/// own tag is prepended to the generic fallback list. Without it, elements
/// promoted by the listener-detection pass that have no id (`selectorHint`
/// empty) could never be re-resolved — the fallback query only matched
/// standard interactive tags, so every ref click on a promoted div failed
/// as stale. The tag comes from the page (`el.tagName.toLowerCase()`), so
/// it is validated before being spliced into a selector.
fn tag_for_query(tag: &str) -> String {
    const GENERIC: &str = "button, a[href], input, textarea, select, [role='button'], [role='link'], [role='checkbox'], [role='radio'], [role='tab'], summary";
    match tag {
        "button" => "button, [role='button'], input[type='submit'], input[type='button']".into(),
        "a" => "a[href], [role='link']".into(),
        "input" => "input".into(),
        "textarea" => "textarea".into(),
        "select" => "select".into(),
        "summary" => "summary".into(),
        other => {
            let is_valid_tag = !other.is_empty()
                && other
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-');
            if is_valid_tag {
                format!("{other}, {GENERIC}")
            } else {
                GENERIC.into()
            }
        }
    }
}
