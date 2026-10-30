# Topic C — 영속/자기학습 메모리, 비용 통제, 모니터링 (ruflo 실제 코드 기준)

범위: 읽기 전용 분석. 경로는 `/Users/psw/Projects/ruflo` 기준 상대경로. 측정 vs 주장은 `docs/reviews/intelligence-system-audit-2026-05-29.md`(이하 audit, v3.10.6 기준, 이후 v3.10.7~9에서 일부 수정)를 따랐다. 이 문서는 읽은 코드에서 확인한 것만 적었고, 실행해서 검증한 것은 없다.

## 0. 한 줄 요약

학습 루프와 메모리 저장은 실제로 돈다(audit.md:12, :24-35). 그러나 **"학습한 결과가 에이전트 컨텍스트에 들어가는 길"은 얇고 대부분 advisory**다. 세션마다 실제로 주입되는 것은 hook이 stdout으로 찍는 텍스트 몇 줄뿐이다. 모델 선택·비용 한도의 강제력은 ruflo 자체가 아니라 (a) ruflo가 직접 띄우는 agent 레코드, (b) Claude Code mod의 `agent.spawn` deny, (c) 외부 gateway에 있다.

## A. 구현되어 있고 shipped CLI/MCP 경로에 연결됨

### A1. 컨텍스트 주입 경로 (가장 중요)
- `.claude/settings.json`의 `UserPromptSubmit` → `.claude/helpers/hook-handler.cjs route`. 이 핸들러가 `intelligence.getContextDetailed(prompt)`를 호출해 결과 텍스트를 `console.log`한다(`hook-handler.cjs:791-802`). Claude Code가 이 stdout을 컨텍스트로 넣는다.
- 검색은 벡터가 아니다. `.claude-flow/data/ranked-context.json`을 읽어 **토큰 커버리지 + IDF 가중 + PageRank 10%**로 점수화하고 `MIN_COVERAGE=0.15`, `TOP_K=5`로 자른다(`.claude/helpers/intelligence.cjs:758-815`). 출력 한 항목은 80자 요약이다(`:827-832`). 프롬프트 전문이 아니라 요약 5줄이 주입 단위다.
- 암묵적 피드백: 직전에 매칭된 패턴이 이번에 다시 매칭되지 않으면 confidence +0.03(`intelligence.cjs:805-812`). 성공 신호가 아니라 "사용자가 계속 작업함"의 추정이다.
- 회상 감사 로그: `.claude-flow/data/recall-log.jsonl`(최대 1MB/1000건, 프롬프트 본문 없이 id·점수·router pick만; `intelligence.cjs:44-46`, `hook-handler.cjs:803-815`).
- 같은 `route` 핸들러가 출력하는 에이전트 추천은 `router.cjs`의 **정적 키워드 표**다. 코드 주석이 스스로 "학습 모델이 아니다, confidence는 휴리스틱 prior"라고 명시한다(`.claude/helpers/router.cjs:4-6`). 이 세션의 hook 출력("Agent: coder, Confidence 60%")이 바로 이 경로다.

### A2. 메모리 저장소와 검색
- MCP `memory_store / retrieve / search / list / stats / import_claude / bridge_status / search_unified`(`v3/@claude-flow/cli/src/mcp-tools/memory-tools.ts:504, 609, 680, 936, 1006, 1185, 1365, 1461`). 저장 위치는 `.swarm/memory.db`(SQLite; 도구 설명에 명시, `:610`).
- AgentDB 백엔드: HNSW 기본값 M=16, efConstruction=200, efSearch=100(`v3/@claude-flow/memory/src/agentdb-backend.ts:124-126`). native hnswlib이 없으면 정확 코사인으로 폴백(`:141`).
- 하이브리드 검색: BM25 + 코사인 선형결합 + MMR(`v3/@claude-flow/cli/src/memory/hybrid-retrieval.ts:1-9`). 이유가 주석에 있다: N<1k에서 MiniLM 코사인이 노이즈가 커서 BM25로 보정.
- Claude Code auto-memory 브리지: `AutoMemoryBridge`가 `~/.claude/projects/<proj>/memory/*.md` ↔ AgentDB를 양방향 동기화한다(`v3/@claude-flow/memory/src/auto-memory-bridge.ts:1-12`). 기본값은 `syncMode: 'on-session-end'`, `minConfidence: 0.7`, `maxIndexLines: 180`(Claude가 MEMORY.md 앞 200줄만 읽기 때문), prune은 `confidence-weighted`(`:171-182`). 호출은 `SessionStart → auto-memory-hook.mjs import`, `Stop → sync`(`.claude/settings.json` hooks).
- 신뢰도 감쇠·가산: LearningBridge는 시간당 0.005 감쇠, 접근당 +0.03, 상한 1.0, 하한 0.1, EWC λ=2000(`learning-bridge.ts:26-37`). 신경 시스템이 없으면 no-op로 퇴화한다(`:5-7`).

### A3. 모델 라우팅 (3-tier)
- 실제 구현은 두 층이다. (1) `ruvector/enhanced-model-router.ts`: Tier1 codemod(`var-to-const`, `remove-console`, `add-logging`), 복잡도 임계 haiku<0.3, sonnet<0.6, 그 이상 opus(`:286-291, :486-545`). (2) `ruvector/model-router.ts`: 어휘 휴리스틱 복잡도 + Thompson Beta-Bernoulli bandit, 복잡도 3구간별 prior, `.swarm/model-router-state.json`에 매 결정 후 저장(`model-router.ts:11-13, :537-538`). audit.md:34가 `model-outcome` 피드백 후 선택이 실제로 이동함을 측정했고, ADR-142/audit.md:115로 prior가 전역→구간별로 바뀌었다.
- `[CODEMOD_AVAILABLE]` / `[TASK_MODEL_RECOMMENDATION]` 문자열은 **MCP `hooks_pre-task`/route 도구의 반환 필드**와 CLI `hooks route` 출력에서 생성된다(`mcp-tools/hooks-tools.ts:1818, 1830`; `commands/hooks.ts:1877, 1889`). Tier1은 `hooks_codemod` MCP 도구로 TypeScript 컴파일러를 써서 실제로 무LLM 변환한다(`hooks-tools.ts:5409` 설명).
- 강제되는 지점은 하나: MCP `agent_spawn`이 ruflo 에이전트 레코드의 `model`을 router로 정한다(`mcp-tools/agent-tools.ts:243-262`, 실패 시 agent type 기본값 `:273`). 이 레코드는 ruflo가 관리하는 agent에만 적용되고, Claude Code의 Task tool로 뜨는 subagent의 모델은 건드리지 않는다.
- 결과 학습: `hooks_model-outcome`이 `recordOutcome`을 호출해 Beta prior를 갱신(`hooks-tools.ts:5273-5293`). verify 경로는 `escalated`도 기록(`:5374-5379`).

### A4. 비용 통제 (ADR-508, 2026-10-29 merged)
- 설계: **귀속은 mod, 강제는 gateway, 연결은 파일**(`v3/docs/adr/ADR-508-cost-ladder-gateway-budgets.md` §2). 예산 파일 `.claude-flow/cost/budget.json`(windows[], hardStop, fallbackModel), 검증기는 CLI(`cost/budget-file.ts`)와 mod(`plugins/ruflo-mods/hooks/cost/file.ts`)에 복제되어 있고 같은 악성 입력 코퍼스로 동일 응답을 요구하는 테스트가 있다(ADR §5).
- 사다리: 50/75/90/100%에서 한 번씩 알림. 100%이고 hardStop이면 `agent.spawn`을 `deny`로 거부(`plugins/ruflo-mods/hooks/cost/index.ts:18-20, :90-94`). **이미 시작된 모델 호출은 못 막고 다음 턴부터 적용**된다(ADR §5 "Ladder").
- gateway 내보내기: `ruflo cost budget export --gateway litellm`이 `budget_limits`/`model_max_budget`/`budget_fallbacks`를 생성, `--apply`는 HTTPS 또는 loopback만, `redirect:'error'`, 15s 타임아웃, 키는 환경변수 이름으로만 전달(ADR §5 `--apply`). 리뷰에서 `//evil.example/` 경로로 master key가 다른 호스트에 갈 수 있는 SSRF성 버그를 잡아 고쳤다는 기록이 있다.
- 귀속은 추정치다. Claude Code가 턴 단위(`session.measure`)로만 비용을 주기 때문에 델타를 그 사이 완료된 tool call 수로 균등 분할한다(ADR §5 "Attribution granularity"). `.claude-flow/cost/attribution.jsonl`(세션당 한 줄, 최신 500개)에 쓰고 `ruflo cost report`가 읽는다. 정확한 요청별 비용은 `turn.step`+가격표가 필요하지만 신뢰 표면이 커져 의도적으로 제외했다.
- 거부 감지는 에러 문구 파싱이다: `reason:'error'`이고 마지막 메시지가 `API Error`로 시작할 때만 LiteLLM 예산 문구를 매칭한다(ADR §5). 취약하지만 모델 본문은 읽지 않도록 범위를 좁혔다.
- `plugins/ruflo-cost-tracker`는 별개의 읽기 전용 원장이다. 각 도구가 이미 쓰는 로그(Claude transcript, Codex rollout)를 한 행 형태로 통일하고, message.id+requestId로 중복 제거, Codex는 누적 total의 델타만 취한다(`scripts/_ledger.mjs:1-20`). "이중 계산 방지 규칙"이 코드 주석으로 문서화된 점이 좋다.

### A5. 모니터링 UI와 데이터 출처
- 상태줄: `.claude/helpers/statusline.cjs`가 `hooks statusline --json`을 위임 호출하고 `.swarm/memory.db`로 벡터 수를 보강, 결과는 `os.tmpdir()` 캐시(`statusline.cjs:195-199, :62, :309-330`). 실패 시 최소 정적 객체로 폴백해 상태줄이 죽지 않는다.
- `ruflo tui`: 같은 뷰(Overview, Cost, Swarm, Events, Missions, AI)를 mod 호스트 대신 Node 호스트에서 그린다. 번들은 CLI 자신의 설치 경로에서만 로드하고 작업 디렉터리에서는 절대 로드하지 않는다(저장소가 공급한 코드를 실행하지 않기 위함). TTY가 아니면 exit 2, `--plain`이면 한 프레임(`commands/tui.ts:1-17`). 번들이 없으면 메시지+exit 1이며 ruflo는 계속 동작한다.
- ruflo-console 원칙: "ruflo 자신의 파일을 읽고, 측정 못 하는 값은 `n/a`로 표시"(`plugins/ruflo-console/README.md:3`). 비용 뷰는 사용량 샘플 20개로 소진 속도를 투영하고(`hooks/cost.ts` `projectSpend`), 예산 설정은 `claude plugin configure ruflo-mods@ruflo --values-stdin`으로 위임한다.
- Mission 상태: `.claude-flow/missions/<id>/events.jsonl`(append-only, 해시 체인), `snapshot.json`은 재생성 가능한 캐시, 미션별 writer lock과 `mkdir` 원자적 id 선점(`v3/@claude-flow/cli/src/missions/store.ts:11-13, :116-120`). 상태기계는 `draft→planned→awaitingAuthorization→queued→running→paused/blocked→completed/failed/cancelled`이며 전이표가 유일한 권위, 관측(observation)은 상태를 바꾸지 않는다(`missions/transitions.ts:1-10`, `schemas.ts:33-40`). 입력은 모두 zod strict 파싱, principal/tenant는 입력에서 받지 않고 서비스가 유도한다(`schemas.ts:1-10`).

## B. 구현은 있으나 기본 경로에 안 붙었거나 선택적

- 신경 라우팅(`CLAUDE_FLOW_ROUTER_NEURAL=1` 삼중 게이트, metaharness knn/krr/fastgrnn)과 `CLAUDE_FLOW_ROUTER_PARALLEL_LOG=1` 병렬 로깅은 opt-in이다(CLAUDE.md "Routing integration"). 승격 기준(quality>2%, cost<1%, latency<5%)은 분석 스크립트로만 판정한다.
- `@claude-flow/memory`가 해석되지 않으면 auto-memory 가져오기 전체가 꺼진다. 이 세션 시작 로그가 정확히 그 상태였다("self-learning imports are DISABLED", `auto-memory-hook.mjs:50-52`). 즉 **학습 계층은 조용히 퇴화하는 선택 의존성**이며 경고만 남기고 세션은 계속된다.
- SONA/MoE/EWC++: MoE(384→128→8)와 SONA WASM(0.0042ms)은 측정으로 실재가 확인됐다(audit.md:29-30). EWC++는 Fisher 정보가 `|w|`·`embedding²` 휴리스틱 대용치다(audit.md:41, 이후 문서에서 정직하게 명시). WASM MicroLoRA는 3.10.9 이후에도 추론 출력이 변하지 않는다(audit.md:121). 즉 "학습은 하지만 추론에 반영되지 않는" 구성요소가 남아 있다.
- 기본 주입 경로(A1)는 AgentDB·HNSW를 쓰지 않는다. `ranked-context.json` 키워드 매칭이므로 HNSW/ONNX는 `memory_search` 도구를 에이전트가 **직접 호출할 때만** 쓰인다.
- ruflo-mods의 `prompt.submit` 훅은 `hook-handler route`를 프로세스 안에서 재구현해 선택적으로 ranked memory까지 주입한다(`plugins/ruflo-mods/hooks/route/index.ts:11-29`). mod가 없으면 위 A1의 settings.json hook이 대신한다.

## C. 문서에만 있거나 열망적

- "HNSW 150x–12,500x": 측정 최대 1.48x였고(audit.md:52), 수정 후에도 N=5k에서 3.2–4.7x, N=20k에서 1.9x가 상한이다(audit.md:100, :120). `CLAUDE.local.md`(저장소 루트, 비추적)에는 아직 옛 수치가 남아 있다.
- Flash Attention 2.49–7.47x: 런타임에 `Math.random()`으로 만든 값이었다(audit.md:53). 제거됐지만 CLAUDE.local.md에 문구가 남아 있다.
- 3-tier 표의 지연/비용(~1ms, ~500ms, $0.0002)은 설계 수치이며 라우터가 측정하지 않는다. `estimatedCost`는 상수(`enhanced-model-router.ts:537`의 0.003 등)다.
- "agent별 자동 라우팅"은 설정 훅에 연결돼 있지 않다. 기본 `route` hook은 모델이 아니라 에이전트 이름만 낸다. `[TASK_MODEL_RECOMMENDATION]`을 따를지는 전적으로 모델의 자율이다.

## D. 통제 지점 정리

| 항목 | 누가 결정 | 강제/권고 | 상태 저장 | 실패 시 |
|---|---|---|---|---|
| 컨텍스트 주입 | hook-handler(키워드 점수) | 권고(텍스트일 뿐) | `.claude-flow/data/*.json(l)` | 모든 예외 삼킴, 프롬프트 차단 안 함 |
| 에이전트/모델 추천 | 정적 표 또는 bandit | 권고 | `.swarm/model-router-state.json` | 라우터 없으면 agent type 기본값 |
| 모델 선택(ruflo agent) | `agent_spawn` | 강제(레코드 한정) | 에이전트 레코드 | 기본값 폴백 |
| 예산 hard stop | mod `agent.spawn` deny | 강제(다음 턴부터, spawn만) | `budget.json`, attribution.jsonl | 파일 불량 시 "예산 없음"(0 아님) |
| 예산 실제 차단 | 외부 gateway | 강제 | gateway | ruflo는 토스트만, 재시도/모델변경 안 함 |
| 미션 전이 | 전이표+zod | 강제 | 해시체인 jsonl | lock, 잘린 마지막 줄은 무시 후 truncate |

## E. 약점

1. 주입이 약하고 평가가 없다. 회상이 도움이 됐는지는 "다음에 또 매칭됨"으로만 추정한다(`intelligence.cjs:805`). 인과 신호가 없다.
2. 학습 계층이 선택 의존성이라 조용히 꺼진다. 사용자는 세션 로그를 읽어야 안다.
3. 비용 hard stop은 모델 호출 도중과 병렬 호출에서 새고, 귀속은 호출 수 균등분할 추정이다.
4. 모델 라우팅은 Claude Code 내부 Task tool을 통제하지 못한다.
5. 측정 정직성 문제의 이력: 난수 속도 지표, mock 임베딩이 ONNX 이름으로 보고됨(audit.md:44), 음수 보상이 양수로 뒤집힘(audit.md:63). 지표가 코드에서 만들어지면 신뢰를 잃는다는 교훈.

## Kaisa에 가져올 아이디어와 피해야 할 점

가져올 것:
- **귀속은 앱, 강제는 외부 경계**, 연결은 검증된 파일 하나(ADR-508). 예산 파일은 불량이면 "예산 없음"이 아니라 거부로 처리하되 0으로 해석하지 않는 규칙, 두 구현의 동일 코퍼스 테스트.
- 비용 사다리(50/75/90/100)와 "다음 턴부터 적용"의 한계를 UI에 솔직히 표시.
- 측정 못 하면 `n/a`로 표시하는 UI 원칙(`ruflo-console` README). 상태줄도 실패 시 정적 폴백.
- 미션 상태: append-only 해시체인 이벤트 로그 + 재생성 가능한 스냅샷 + 전이표를 단일 권위로 + 관측은 상태를 바꾸지 않음. Rust에서는 enum 전이 테이블과 `replay` 순수 함수로 그대로 옮기기 좋다.
- 컨텍스트 주입 단위를 "80자 요약 N줄 + 감사 로그(본문 없음)"로 작게, 그리고 나중에 효과를 측정할 수 있게 회상 id와 점수를 남기는 방식.
- 라우팅은 휴리스틱 + 구간별 Beta bandit처럼 작고 설명 가능하게. prior를 작업 유형별로 분리(전역 prior는 한 유형의 실패가 전체를 오염시킨다).
- 외부 입력(설정, 예산, URL)은 보안 예산을 두고 검증: 악성 코퍼스 테스트, 리다이렉트 금지, 키는 argv 금지.

피할 것:
- 학습 의존성을 선택 설치로 두고 조용히 비활성화하지 말 것. 핵심 능력이면 필수이거나, 꺼졌을 때 UI에 상시 표시.
- 성능 배수·속도 수치를 문서 문자열이나 난수로 만들지 말 것. 모든 지표는 벤치마크 스크립트 출력에서만.
- "추천 텍스트를 찍으면 모델이 따를 것"이라는 가정. 강제할 수 있는 건 ruflo가 직접 spawn하는 에이전트뿐이다. Kaisa는 오케스트레이터가 직접 에이전트를 띄우므로 모델 선택을 **강제 경로**에 둘 수 있다는 점이 구조적 이점.
- 구현이 4개 층(hook-handler 정적, MCP 라우터, mod, 신경 라우터)으로 중복돼 있어 어느 경로가 실제 동작하는지 불분명. 경로 하나, 정책 하나.
- 임베딩 mock 폴백을 실제 모델 이름으로 보고하는 것, CLI 플래그 파서가 음수 보상을 뒤집는 것 같은 "학습 방향" 버그. 보상 부호와 백엔드 종류는 타입으로 구분하고 단위 테스트로 고정.
- 요청 단위 비용을 호출 수로 나누는 추정에 의존하는 설계. 가능하면 처음부터 요청별 usage를 직접 받는 이벤트를 설계에 포함.
