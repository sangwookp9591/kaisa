//! F10's ko/en message catalog (NFR-05). Ledger text and stable engine codes are never translated.

use std::fmt::Display;

macro_rules! catalog {
    ($($name:ident => ($english:literal, $korean:literal)),+ $(,)?) => {
        #[derive(Clone, Copy)]
        pub(super) enum Message { $($name),+ }
        #[cfg(test)]
        pub(super) const ALL: &[Message] = &[$(Message::$name),+];
        pub(super) fn text_for(message: Message, language: &str) -> &'static str {
            match (message, language) {
                $((Message::$name, "ko") => $korean,
                (Message::$name, _) => $english),+
            }
        }
    };
}

catalog! {
    Help => ("↑/↓ or j/k  select (details: scroll)\nEnter  details    Tab  switch pane\nn  needs-you filter    f  failed filter\n/  search    g  new goal (Enter starts it)\nr  refresh    ?  help    q / Esc / Ctrl-C  quit\nPgUp / PgDn  scroll details\n\nTake over, steer, pause, undo, approve: not available yet.\nThese actions do nothing. This home never approves or merges work.\n\nRefresh reads the ledger; it never starts or retries an agent.\nUnlinked attempts stay separate until the ledger records their run.\nCosts are provider-reported; n/a costs are excluded from totals.\n--plain prints one complete text frame and exits.\n\nPress ? or Enter to close help.",
        "↑/↓ 또는 j/k  선택 (상세 창: 스크롤)\nEnter  상세 보기    Tab  창 전환\nn  사람을 기다리는 작업    f  실패한 작업\n/  검색    g  새 목표 (Enter로 시작)\nr  새로고침    ?  도움말    q / Esc / Ctrl-C  종료\nPgUp / PgDn  상세 내용 스크롤\n\n인계, 방향 변경, 일시 중지, 실행 취소, 승인: 아직 지원하지 않아요.\n이 키는 아무 작업도 하지 않아요. 이 화면에서 승인하거나 병합하지 않아요.\n\n새로고침은 기록만 읽고 에이전트를 시작하거나 재시도하지 않아요.\n실행 연결이 기록되지 않은 시도는 따로 표시해요.\n비용은 제공자가 보고한 값이며, 알 수 없는 비용은 합계에서 제외해요.\n--plain은 전체 텍스트 화면을 한 번 출력하고 종료해요.\n\n? 또는 Enter로 도움말을 닫으세요."),
    CommandUsage => ("usage: kaisa tui [--plain]", "사용법: kaisa tui [--plain]"),
    Error => ("kaisa tui: {}", "kaisa tui: {}"),
    LedgerError => ("kaisa tui: the ledger cannot be read: {}", "kaisa tui: 기록을 읽을 수 없어요: {}"),
    PlainTitle => ("KAISA · terminal home\nNeeds you", "KAISA · 터미널 홈\n사람을 기다려요"),
    MoreNeeds => ("+{} more · n to view all", "+{}개 더 · n으로 모두 보기"),
    NoNeeds => ("No recorded NeedsPerson gates.", "사람을 기다리는 게이트가 없어요."),
    Runs => ("Goals / runs", "목표 / 실행"),
    Empty => ("nothing has run yet", "아직 실행한 작업이 없어요"),
    EmptyInteractive => ("nothing has run yet · g to start a goal", "아직 실행한 작업이 없어요 · g로 목표를 시작하세요"),
    NoMatches => ("No matching goals · toggle filter or clear /search", "일치하는 목표가 없어요 · 필터를 끄거나 /검색을 지우세요"),
    Choose => ("Select a goal or attempt to read its evidence.", "목표나 시도를 선택하면 근거를 볼 수 있어요."),
    GoalNeeded => ("A new goal needs text.", "새 목표를 입력하세요."),
    All => ("All", "전체"),
    Needs => ("Needs you", "사람을 기다려요"),
    Failed => ("Failed", "실패"),
    NeedsTitle => (" Needs you · exit 3 ", " 사람을 기다려요 · 종료 코드 3 "),
    RunsTitle => (" Goals / runs · {} · /{} ", " 목표 / 실행 · {} · /{} "),
    DetailsTitle => (" Details · Enter / Tab · PgUp/PgDn ", " 상세 · Enter / Tab · PgUp/PgDn "),
    KeysTitle => (" Keys / capabilities ", " 키 / 지원 기능 "),
    Header => (" / terminal home    ·    ledger refresh 1s", " / 터미널 홈    ·    기록 새로고침 1초"),
    Footer => ("n needs you · f failed · / search · g goal · r refresh · ? help · q quit\n{}", "n 대기 · f 실패 · / 검색 · g 목표 · r 새로고침 · ? 도움말 · q 종료\n{}"),
    SearchPrompt => ("Search: {}▏  · Enter finishes · Esc quits", "검색: {}▏  · Enter 완료 · Esc 종료"),
    GoalPrompt => ("New goal: {}▏\nEnter starts kaisa in this repository · Esc quits", "새 목표: {}▏\nEnter로 현재 저장소에서 kaisa를 시작해요 · Esc 종료"),
    Started => ("Started PID {} · log: {}", "PID {} 시작 · 로그: {}"),
    StartError => ("Goal could not start: {}", "목표를 시작하지 못했어요: {}"),
    RefreshError => ("Refresh failed; showing previous frame: {}", "새로고침 실패, 이전 화면을 표시해요: {}"),
    ProcessExit => ("Goal process {} · log: {}", "목표 프로세스 {} · 로그: {}"),
    ProcessUnknown => ("Goal status unknown: {} · log: {}", "목표 상태를 확인할 수 없어요: {} · 로그: {}"),
    Totals => ("Reported: {} tokens · ${} · {} cost(s) n/a, excluded", "보고된 사용량: {} 토큰 · ${} · 비용 미상 {}건 제외"),
    CompactUsage => ("tokens {} · cost {}", "토큰 {} · 비용 {}"),
    UsageUnknown => ("tokens n/a · cost n/a (not reported)", "토큰 n/a · 비용 n/a (보고되지 않음)"),
    ProviderNoCost => ("n/a (provider gives no cost)", "n/a (제공자가 비용을 보고하지 않음)"),
    Usage => ("tokens {}/{} · cost {}", "토큰 {}/{} · 비용 {}"),
    GateUnknown => ("gate not recorded", "게이트가 기록되지 않음"),
    RoleUnknown => ("role not recorded", "역할이 기록되지 않음"),
    GoalUnknown => ("goal not recorded yet", "목표가 아직 기록되지 않음"),
    TranscriptUnreadable => ("transcript not readable yet", "기록을 아직 읽을 수 없음"),
    Reduced => ("reduced / {}: {}", "reduced / {}: {}"),
    Confined => ("confined (platform limits still apply)", "confined (플랫폼 제한은 여전히 적용됨)"),
    Containment => ("containment: {}", "격리 상태: {}"),
    Unlinked => ("Attempt is not linked to a verification run yet; no outcome inferred.", "시도의 검증 실행 연결이 아직 기록되지 않았어요. 결과를 추정하지 않아요."),
    Revision => ("revision {} (developer)", "수정 {} (developer)"),
    GoalDetails => ("Goal: {}\nRun: {}\nGate: {}\nReason: {}\n", "목표: {}\n실행: {}\n게이트: {}\n이유: {}\n"),
    Repository => ("Repository: {}\n", "저장소: {}\n"),
    Branch => ("Result branch: kaisa/{}\n", "결과 브랜치: kaisa/{}\n"),
    NoMerge => ("Not delivered; no merge is offered.\n", "전달되지 않은 결과라 병합을 제안하지 않아요.\n"),
    Review => ("Review {}: valid={} approve={} findings={}\n", "리뷰 {}: 유효={} 승인={} 지적={}\n"),
    Check => ("Check: {}\n  exit: {} · tree: {}\n", "검사: {}\n  종료: {} · 트리: {}\n"),
    Finding => ("Finding: {}\n", "지적: {}\n"),
    AttemptDetails => ("\n{} · {}\nTask: {}\nExecution: {} · cleanup: {}\n{}\n{}\nTranscript tail:\n{}\n", "\n{} · {}\n작업: {}\n실행 상태: {} · 정리: {}\n{}\n{}\n기록 끝부분:\n{}\n"),
}

pub(super) fn text(message: Message) -> &'static str {
    text_for(message, language(|name| std::env::var(name).ok()))
}

/// Substitute only catalog placeholders, never placeholders inside user-provided arguments.
pub(super) fn format(message: Message, arguments: &[&dyn Display]) -> String {
    let mut pieces = text(message).split("{}");
    let mut output = pieces.next().unwrap_or_default().to_owned();
    for (piece, argument) in pieces.zip(arguments) {
        output.push_str(&argument.to_string());
        output.push_str(piece);
    }
    output
}

/// An explicit KAISA_LANG wins; otherwise use POSIX locale precedence.
pub(super) fn language(lookup: impl Fn(&str) -> Option<String>) -> &'static str {
    let selected = lookup("KAISA_LANG")
        .or_else(|| {
            ["LC_ALL", "LC_MESSAGES", "LANG"]
                .into_iter()
                .filter_map(lookup)
                .find(|value| !value.is_empty())
        })
        .unwrap_or_default();
    if selected.to_ascii_lowercase().starts_with("ko") {
        "ko"
    } else {
        "en"
    }
}
