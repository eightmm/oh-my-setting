//! Canonical renderer labels; these never change admitted facts.
pub(crate) fn label(lang: &str, key: &str) -> &'static str {
    match (lang, key) {
        ("ko", "empty") => "- 기록 없음",
        ("ko", "overview") => "한눈에 보기",
        ("ko", "overview_line") => {
            "기록 %d · 검증 %d · 미검증 %d · 실패/보류 %d · 의사결정 %d · Blocker %d · 다음 작업 %d"
        }
        ("ko", "context") => "관련 작업",
        ("ko", "truncated") => {
            "- … 표시 한도로 상세 항목 일부 생략; 전체 내용은 로컬 Work Journal 파일에 있음"
        }
        ("ko", "daily_progress") => "핵심 진전",
        ("ko", "daily_by_project") => "프로젝트별 작업",
        ("ko", "verified") => "검증된 것",
        ("ko", "unverified") => "아직 검증되지 않은 것",
        ("ko", "failed") => "실패하거나 보류한 접근",
        ("ko", "decisions") => "의사결정",
        ("ko", "blockers") => "Blockers",
        ("ko", "next") => "다음 우선순위",
        ("ko", "work") => "작업",
        ("ko", "evidence") => "관련 evidence",
        ("ko", "result") => "결과",
        ("ko", "interpretation") => "해석",
        ("ko", "weekly_by_project") => "프로젝트별 진전",
        ("ko", "weekly_verified") => "완료하거나 검증한 작업",
        ("ko", "recurring_blockers") => "반복 Blockers",
        ("ko", "weekly_decisions") => "주요 의사결정",
        ("ko", "trends") => "비교 가능한 실험 추세",
        ("ko", "weekly_next") => "다음 주 우선순위",
        ("ko", "repeated") => "반복 %d회",
        ("ko", "sessions") => "세션",
        ("ko", "sessions_restore") => {
            "복원: `oms session-handoff list`로 해당 세션의 핸드오프를 찾을 수 있음"
        }
        ("ko", "digest_completed") => "오늘 끝낸 것",
        ("ko", "digest_failed") => "실패·막힌 것",
        ("ko", "digest_next") => "다음",
        ("ko", "digest_details") => "상세 기록",
        ("ko", "digest_decisions") => "의사결정",
        ("ko", "digest_reviews") => "리뷰",
        ("ko", "digest_commits") => "커밋",
        ("ko", "digest_ci") => "CI",
        ("ko", "digest_no_records") => "- 기록 없음",
        ("ko", "digest_review_passed") => "통과",
        ("ko", "digest_review_failed") => "실패",
        ("ko", "digest_review_count") => "%d회 검토",
        ("ko", "digest_review_unlinked") => "리뷰 %s — %s",
        ("ko", "digest_review_unlinked_failure") => "리뷰 실패: %s%s",
        ("ko", "digest_review_linked") => "%s — 리뷰 %s%s",
        ("ko", "digest_review_linked_failure") => "리뷰 실패: %s%s",
        ("ko", "digest_ci_for_commit") => "- CI %s (커밋 %s)",
        ("ko", "digest_ci_detail") => "- 상세 CI 결과: %s (커밋 %s)",
        ("ko", "digest_ci_unlinked") => "- CI %s",
        ("ko", "digest_ci_success_line") => "- %s (커밋 %s, CI 성공)",
        ("ko", "digest_commit_note") => " (커밋 %s%s)",
        ("ko", "digest_ci_status_suffix") => " (CI %s)",
        ("ko", "digest_commit_line") => "- %s (커밋 %s%s)",
        ("ko", "digest_status_completed") => "완료",
        ("ko", "digest_status_accepted") => "수락",
        ("ko", "digest_status_failed") => "실패",
        ("ko", "digest_status_rejected") => "거부",
        ("ko", "digest_status_success") => "성공",
        ("ko", "digest_status_failure") => "실패",
        ("ko", "digest_status_cancelled") => "취소",
        ("ko", "digest_status_timed_out") => "시간 초과",
        ("ko", "digest_status_startup_failure") => "시작 실패",
        ("ko", "digest_status_unknown") => "알 수 없음",
        ("ko", "digest_verification_failed") => "검증 실패",
        ("ko", "digest_failure_status") => "상태: %s",
        ("ko", "digest_failure_exit") => "종료 코드: %s",
        ("en", "empty") => "- none recorded",
        ("en", "overview") => "At a glance",
        ("en", "overview_line") => {
            "Events %d · verified %d · unverified %d · failed/parked %d · decisions %d · blockers %d · next actions %d"
        }
        ("en", "context") => "related work",
        ("en", "truncated") => {
            "- … Some detail was omitted at the display limit; the complete summary remains in the local Work Journal file"
        }
        ("en", "daily_progress") => "Key progress",
        ("en", "daily_by_project") => "Work by project",
        ("en", "verified") => "Verified",
        ("en", "unverified") => "Not yet verified",
        ("en", "failed") => "Failed or parked approaches",
        ("en", "decisions") => "Decisions",
        ("en", "blockers") => "Blockers",
        ("en", "next") => "Next priorities",
        ("en", "work") => "work",
        ("en", "evidence") => "related evidence",
        ("en", "result") => "result",
        ("en", "interpretation") => "interpretation",
        ("en", "weekly_by_project") => "Progress by project",
        ("en", "weekly_verified") => "Completed or verified",
        ("en", "recurring_blockers") => "Recurring blockers",
        ("en", "weekly_decisions") => "Key decisions",
        ("en", "trends") => "Comparable metric trends",
        ("en", "weekly_next") => "Next week priorities",
        ("en", "repeated") => "repeated %d times",
        ("en", "sessions") => "Sessions",
        ("en", "sessions_restore") => {
            "restore: `oms session-handoff list` locates each session's handoff"
        }
        ("en", "digest_completed") => "Completed",
        ("en", "digest_failed") => "Failed or blocked",
        ("en", "digest_next") => "Next",
        ("en", "digest_details") => "Details",
        ("en", "digest_decisions") => "Decisions",
        ("en", "digest_reviews") => "Reviews",
        ("en", "digest_commits") => "Commits",
        ("en", "digest_ci") => "CI",
        ("en", "digest_no_records") => "- none recorded",
        ("en", "digest_review_passed") => "passed",
        ("en", "digest_review_failed") => "failed",
        ("en", "digest_review_count") => "%d reviews",
        ("en", "digest_review_unlinked") => "Review %s — %s",
        ("en", "digest_review_unlinked_failure") => "Review failed: %s%s",
        ("en", "digest_review_linked") => "%s — review %s%s",
        ("en", "digest_review_linked_failure") => "Review failed: %s%s",
        ("en", "digest_ci_for_commit") => "- CI %s for commit %s",
        ("en", "digest_ci_detail") => "- CI result: %s for commit %s",
        ("en", "digest_ci_unlinked") => "- CI %s",
        ("en", "digest_ci_success_line") => "- %s (commit %s, CI passed)",
        ("en", "digest_commit_note") => " (commit %s%s)",
        ("en", "digest_ci_status_suffix") => " (CI %s)",
        ("en", "digest_commit_line") => "- %s (commit %s%s)",
        ("en", "digest_status_completed") => "completed",
        ("en", "digest_status_accepted") => "accepted",
        ("en", "digest_status_failed") => "failed",
        ("en", "digest_status_rejected") => "rejected",
        ("en", "digest_status_success") => "success",
        ("en", "digest_status_failure") => "failure",
        ("en", "digest_status_cancelled") => "cancelled",
        ("en", "digest_status_timed_out") => "timed out",
        ("en", "digest_status_startup_failure") => "startup failure",
        ("en", "digest_status_unknown") => "unknown",
        ("en", "digest_verification_failed") => "verification failed",
        ("en", "digest_failure_status") => "status: %s",
        ("en", "digest_failure_exit") => "exit %s",
        ("en", _) => "",
        (_, key) => label("en", key),
    }
}

pub(crate) fn format(template: &str, values: &[String]) -> String {
    let mut output = String::new();
    let mut values = values.iter();
    let mut chars = template.chars();
    while let Some(ch) = chars.next() {
        if ch == '%' {
            match chars.next() {
                Some('s' | 'd') => {
                    if let Some(value) = values.next() {
                        output.push_str(value);
                    }
                }
                Some('%') => output.push('%'),
                Some(other) => {
                    output.push(ch);
                    output.push(other);
                }
                None => output.push(ch),
            }
        } else {
            output.push(ch);
        }
    }
    output
}
