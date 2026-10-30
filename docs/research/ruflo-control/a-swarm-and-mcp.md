# Ruflo Topic A — swarm/agent 조정 제어와 MCP (실제 코드 기준)

경로는 모두 `v3/@claude-flow/` 기준 상대 경로입니다. 이 분석은 읽기 전용이며 repo는 수정하지 않았습니다.

## 0. 핵심 결론

Ruflo의 swarm, agent, task, hive-mind MCP 도구는 **JSON 파일에 기록하는 장부(ledger)** 입니다. 프로세스를 띄우거나 작업을 실행하거나 정책을 강제하는 실행기가 아닙니다. 실제 실행은 세 곳에서 일어납니다.

- Claude Code의 Task 도구
- `agent_execute`가 호출하는 LLM API
- 별도 CLI가 spawn하는 `claude -p` / `codex exec` 자식 프로세스

topology, 합의, anti-drift는 대부분 메타데이터이거나 권고입니다. 이 점은 코드 주석과 도구 응답이 스스로 인정합니다 (`cli/src/mcp-tools/coordination-tools.ts:709-742`, `cli/src/mcp-tools/agent-tools.ts:479-481`).

## A. 구현되어 있고 shipped CLI/MCP 경로에 연결된 것

### A1. MCP 서버와 도구 레지스트리

- **등록**: 도구는 정적 배열로 합쳐져 `TOOL_REGISTRY` Map에 들어갑니다 (`cli/src/mcp-client.ts:131-141`, 도구 spread는 `:140-166`). 동적 발견이나 플러그인 훅은 없습니다.
- **stdio 경로**: `cli/src/mcp-server.ts:961-1010`이 `tools/list`와 `tools/call`을 직접 처리합니다. 호출 순서는 `hasTool` → (옵션) 정책 체크 → `callMCPTool`입니다.
- **중앙 관문**: `callMCPTool`이 모든 호출을 `authorizeMcpTool`(ADR-324 정책 엔진)에 통과시킵니다. 결과가 `allowed`가 아니면 throw하고 receipt를 남깁니다 (`cli/src/mcp-client.ts:255-296`).
- **출력 스크린**: 결과 인젝션 스크린은 `CLAUDE_FLOW_STRICT_GUARDRAIL=true`일 때만 동작합니다. 기본값은 off입니다 (`cli/src/mcp-client.ts:312-315`).
- **HTTP 경로**: `@claude-flow/mcp`의 `createMCPServer`를 쓰며, 여기에 rate-limit, token bucket, auth, session이 있습니다 (`cli/src/mcp-server.ts:1107-1175`, `mcp/src/types.ts:98-123`).
- **스키마 노출 제어**: `filterAdvertisedMcpTools`가 카테고리/접두어로 `tools/list`를 줄입니다 (`cli/src/mcp-server.ts:353-366`). 스키마 토큰 오버헤드 추정도 있습니다 (`:377`).
- **정책 강제는 opt-in입니다**: `RUFLO_MCP_ENFORCE_POLICY=1`일 때만 `evaluateToolCall`이 동작하며, fail-closed입니다. 정책 파일이 없거나 감사 로그 쓰기에 실패하면 호출을 거부합니다 (`cli/src/mcp-tools/policy-enforcer.ts:1-60`). `maxToolCallsPerTurn`은 롤링 윈도우(기본 60초)입니다. 이 코드의 주석에 따르면 `@claude-flow/mcp`의 HTTP/WebSocket 경로에는 이 정책이 적용되지 않습니다.
- **메모**: `@claude-flow/mcp`는 구조상 일반 MCP 프레임워크입니다 (`mcp/src/{server,tool-registry,session-manager,task-manager,sampling}.ts`). CLI는 HTTP 모드에서만 이를 씁니다.

### A2. swarm_init

- **상태 저장**: 단일 JSON 파일 `.claude-flow/swarm/swarm-state.json`입니다. 쓰기는 temp 파일 후 rename으로 하고, `swarm-state.lock`을 `wx`로 열어 락을 잡습니다. 10초가 지난 stale lock은 삭제합니다 (`cli/src/mcp-tools/swarm-tools.ts:33-36,130-175`).
- **topology**: 화이트리스트는 `hierarchical, mesh, hierarchical-mesh, ring, star, hybrid, adaptive, pheromone-adaptive`입니다 (`:198-200`).
  - `swarm_init`은 값을 검증해 `SwarmState.topology`로 저장할 뿐입니다 (`:272-334`).
  - 이 저장값을 읽어 메시지 라우팅이나 권한을 바꾸는 코드는 확인하지 못했습니다. topology는 라벨입니다.
- **maxAgents**: `[1,50]`으로 clamp합니다 (`:273`). 이후 `agent_spawn`에서 이 값을 검사하는 곳은 없습니다 (`cli/src/mcp-tools/agent-tools.ts`에 `maxAgents` 참조 0건). 즉 상한은 **swarm_health의 정보 메시지**일 뿐입니다 (`swarm-tools.ts:584`).
- **orphan 정리**: `reconcileOrphanSwarms`가 `pid`가 죽었거나, pid가 없고 `updatedAt`이 24시간 넘게 stale인 swarm을 `terminated`로 바꿉니다 (`:94-130`). 일반 CLI 호출은 즉시 종료하므로 기본은 PID를 기록하지 않습니다 (`:317-320`).
- **pheromone-adaptive**: 에이전트별 EMA 적합도로 keep/suspend/reactivate를 정하는 선택형 모드입니다. 기본은 dry-run입니다 (`:283-294,344-392`, `cli/src/services/pheromone-adaptive.ts`).

### A3. agent_spawn / agent_execute

- **agent_spawn**: `agent_store`(JSON)에 레코드를 쓰고, swarm 스토어의 `agents[]` 배열에 id를 중복 없이 push합니다 (`agent-tools.ts:285-440`). 이후 모델 라우팅 결과를 붙이고, 그래프 노드와 선택적 COW 메모리 브랜치를 best-effort로 만듭니다. **프로세스는 만들지 않습니다.** 응답의 `status: 'registered'`와 note가 이를 명시합니다 (`:470-481`).
- **agent_execute**: 등록된 agent의 모델로 LLM API(Anthropic/OpenRouter/Ollama)를 `fetch`로 직접 호출합니다. 기본 타임아웃은 60초, `max_tokens` 기본값은 1024입니다 (`cli/src/mcp-tools/agent-execute-core.ts:251-270`). 이 경로에는 파일 편집이나 bash 같은 도구가 없는 단발 completion입니다. 결과는 taskCount와 lastResult에 반영됩니다.
- **agent_health**: `agent_execute` 마지막 20회의 API 성공률과 지연만 측정합니다. 코드가 직접 "프로세스 liveness는 관측하지 않는다"고 적습니다 (`agent-tools.ts:836-906`).
- **agent_logs**: 마지막 결과만 합성해 반환합니다. 실제 로그가 아닙니다 (`:1002`).

### A4. task_*

- **저장**: 작업은 `.swarm` 쪽 스토어에 있고, 상태는 `pending → in_progress → completed/failed/cancelled`입니다 (`cli/src/mcp-tools/task-tools.ts:127-160,349-370`).
- **task_assign**: `assignedTo`를 기록하고, 해당 agent의 상태를 `busy`로 바꿔 `currentTask`를 세팅합니다. 그리고 pending 작업을 자동으로 in_progress로 전이합니다 (`:463-527`).
- **강제하지 않는 것들**: agent 존재 여부, 동시 작업 수 상한, 클레임(claims) 보유 여부, 의존성 순서는 검사하지 않습니다. task_assign은 순수 상태 기록입니다.
- **MCP 도구 이름**: `task_orchestrate`라는 MCP 도구는 이 repo에서 찾지 못했습니다. 대신 `coordination_orchestrate`가 있습니다.

### A5. 실제 프로세스를 띄우는 경로 (shipped)

- **헤드리스 worker (daemon)**: `claude --print --output-format json`을 `spawn`합니다. 프롬프트는 stdin으로 넘기고, 자식은 `detached`로 띄워 process group 단위로 kill합니다. worker별 타임아웃은 5~10분이고 동시 실행은 `maxConcurrent`(기본 2)로 제한합니다 (`cli/src/services/headless-worker-executor.ts:338-386,1395-1440`, `cli/src/services/worker-daemon.ts:344`). 이것이 repo에서 확인한 실제 동시성 제어 중 가장 단단한 것입니다.
- **Dual-mode 오케스트레이터**: `claude -p` 또는 `codex exec --sandbox workspace-write|read-only`를 spawn합니다. `worktreePath` 격리(cwd), `maxConcurrent`(기본 4), `maxWriters`(기본 2), `maxOutputBytes`(1MiB), worker별 서명 토큰(선택)이 있습니다 (`codex/src/dual-mode/orchestrator.ts:90-110,214-262`). 쓰기 작업자를 제한하고 worktree를 분리하는 것은 CLAUDE.md의 "한 worktree에 writer 하나" 규칙과 같은 방향입니다.
- **`hive-mind spawn --claude`**: 프롬프트 파일을 만들어 Claude를 띄웁니다. `--dangerously-skip-permissions`는 명시 플래그가 있을 때만 붙습니다 (`cli/src/commands/hive-mind.ts:287-302`).

## B. 구현되어 있지만 CLI/MCP 경로에는 연결되지 않았거나 선택 사항

- **`@claude-flow/swarm` 패키지**: 다음 컴포넌트가 실제로 존재합니다.
  - `UnifiedSwarmCoordinator`: 15 에이전트 queen/도메인 계층 (`swarm/src/unified-coordinator.ts:1-60`)
  - `TopologyManager`
  - `MessageBus`
  - `AgentPool`
  - Raft 합의 엔진 (`swarm/src/consensus/raft.ts`)
  - `swarm-hub`, `federation-hub`

  그러나 `cli/package.json`은 이 패키지에 의존하지 않고, `cli/src`에서 `@claude-flow/swarm`을 참조하는 곳은 `capability-brain.ts`의 문자열 정도입니다. 즉 **메시지 버스와 진짜 토폴로지는 MCP 도구가 쓰는 코드가 아닙니다.** 이 패키지에서 `child_process`를 호출하는 곳도 없습니다.
- **claims**: `claims_claim`은 이미 선점된 이슈를 거부하는 배타적 잠금입니다 (`cli/src/mcp-tools/claims-tools.ts:128-150`).
  - `claims_steal`은 먼저 `mark-stealable`된 것만, `preferredTypes` 조건을 지키며 훔칠 수 있습니다 (`:564-625`).
  - `ttlMs`는 `expiresAt`만 기록하고 **어디서도 강제하지 않습니다.** 코드 주석이 직접 "Informational only"라고 밝힙니다 (`:110-112`). 따라서 lease가 아니라 만료 없는 락입니다.
  - claims를 `task_assign`이나 `agent_spawn`과 연결하는 코드는 없습니다. 이슈 단위 조정 보드로 따로 존재합니다.
- **정책 강제**: `RUFLO_MCP_ENFORCE_POLICY` 등은 env 게이트(opt-in)입니다 (A1 참조).
- **pheromone-adaptive 모드**: dry-run 기본값의 선택형입니다 (A2 참조).
- **agent_pool**: warm pool 레코드를 관리합니다 (`agent-tools.ts:709`). 실제 프로세스 풀이 아닙니다.
- **외부 MCP 서버**: `ruv-swarm`과 `flow-nexus`는 별도 npm 패키지로, 선택 등록입니다 (`cli/src/init/mcp-generator.ts:81-135`).

## C. 문서에만 있거나 포부 수준인 것

- **"100+ agents"**: 코드에는 이 수치를 만드는 경로가 없습니다.
  - `swarm_init`의 `maxAgents`는 50으로 clamp됩니다 (`swarm-tools.ts:273`). `hive-mind_spawn`은 호출당 20으로 cap합니다 (`hive-mind-tools.ts:415`).
  - 레코드를 반복 생성하면 개수는 늘릴 수 있지만 모두 레코드입니다.
  - 진짜 동시 프로세스는 위 A5의 `maxConcurrent`(2~4)가 한계입니다. 규모의 실체는 레코드 개수이지 프로세스 개수가 아닙니다.
- **anti-drift**: "drift"를 감지하거나 막는 코드는 MCP 도구와 swarm 명령에서 찾지 못했습니다. 실제 대응물은 세 가지입니다.
  - 선택한 topology 문자열
  - `swarm_status`에 보이는 에이전트 수
  - **동일 명령 연속 실패 3회 warn, 5회 block**을 감지하는 `tool-loop-guardrail` (`cli/src/mcp-tools/tool-loop-guardrail.ts:1-60`). 이것은 drift가 아니라 루프 감지이고, 기본은 advisory이며 pre-command 훅에서만 호출됩니다 (`hooks-tools.ts:13,1156`).
- **gossip / crdt 합의**: `hive-mind_init`의 enum에는 있습니다 (`hive-mind-tools.ts:23,476`). 그러나 투표 로직 타입은 `bft|raft|quorum`뿐이고 (`:56`), gossip과 crdt의 필요 표수 계산은 default 분기(과반)로 떨어집니다 (`:98-114`). 이름만 있는 것입니다.
- **`coordination_orchestrate`**: 도구가 직접 `executor: 'none'`, "실행하지 않고 기록만 한다"고 답합니다 (`coordination-tools.ts:735-760`). 같은 파일의 sync도 가짜 50ms 대기를 제거한 이력이 있습니다 (`:320` 주석).
- **swarm scale**: `swarm scale` 명령은 `currentAgents` 대비 delta를 출력만 하고 실제로 spawn하거나 stop하지 않습니다 (`cli/src/commands/swarm.ts:940-975`).
- **swarm start / v3 coordinate**: 이 명령은 `swarm_init`만 호출하고, 코드가 직접 "실제 실행은 Task 도구나 hive-mind spawn --claude가 한다"고 안내합니다 (`cli/src/commands/swarm.ts:660-700,1036-1046`).

## D. 제어점 분석

- **합의(hive-mind)**: 이 합의는 **실제 분산 합의가 아니라 한 파일 안의 투표 장부**입니다. 구현된 규칙은 다음과 같습니다.
  - 필요 표수: bft는 floor(2n/3)+1, raft와 quorum 기본은 과반입니다 (`hive-mind-tools.ts:86-115`).
  - raft는 term당 pending 제안 1개이며, 중복 제안을 거부하고 `timeoutMs` 기본값은 30초입니다 (`:820-855`).
  - 투표자는 등록된 worker여야 하고(Sybil 방지), 중복 표는 거부됩니다. bft에서 충돌 표가 오면 그 표를 삭제하고 Byzantine으로 기록합니다 (`:880-945`).
  - 리더 선출이나 로그 복제는 없고, queen은 init 때 지정되는 필드입니다 (`:536-560`).
- **원격 호출 인증**: 원격(HTTP) 호출자는 hive 토큰이나 bootstrap secret이 있어야 합니다. 로컬 stdio/CLI 호출자는 자격 증명이 필요 없습니다 (`:265-282`). `hive-mind_spawn`도 fail-closed 게이트를 거칩니다 (`:391-407`).
- **상태 위치**:
  - `.claude-flow/swarm/swarm-state.json`: swarm 상태
  - `.claude-flow/agents.json`: hive worker (`agent-tools.ts:21-24,90-112`)
  - `.swarm/` 스토어: task, agent
  - claims 스토어: claim
  - hive 상태 파일: 합의 이력
  - SQLite `.swarm/memory.db`: 일부 task와 메모리 (`task-tools.ts` 설명)
- **Claude Code / Codex 연동 방식**: Claude Code는 `claude mcp add claude-flow -- npx -y ruflo@latest mcp start`로 등록하는 MCP 클라이언트이고, 실행 주체는 Claude Code의 Task 도구입니다. Codex는 `RUFLO_MCP_SERVER_NAME='ruflo'` 같은 설정으로 `codex` 쪽 MCP 설정에 같은 서버를 등록합니다 (`codex/src/mcp-config.ts:1-12`). 두 플랫폼 모두 같은 장부를 쓰며, 교차 검증은 dual-mode 오케스트레이터가 두 CLI를 spawn해서 이루어집니다.
- **에이전트 간 메시징**: MCP 도구에 에이전트 간 메시지 전송 도구는 없습니다. `swarm-tools.ts`의 `communicationProtocol: 'message-bus'`는 config 문자열일 뿐입니다 (`:310`). 실제 메시지 전달은 Claude Code 쪽 SendMessage 같은 외부 기능이나 공유 메모리 namespace로 하며, 이는 Ruflo가 아니라 호스트가 제공합니다. `swarm` 패키지의 `MessageBus`는 B에서 말한 대로 미연결입니다.
- **실패 처리**: 파일 락의 stale 회수(10초), orphan swarm 재조정(24시간 또는 PID), 헤드리스 worker의 process-group kill과 타임아웃, 정책 모드의 fail-closed가 있습니다. 반면 MCP 레코드가 실제 프로세스 상태와 어긋나는 것(좀비 agent 레코드)을 정리하는 곳은 없습니다. `agent_health` 자신이 "등록은 실행의 증거가 아니다"라고 말합니다.

## 약점 요약

1. 상한(`maxAgents`)과 topology, 합의가 실행 경로를 통제하지 않습니다. 이름과 실제가 다릅니다.
2. claims에 TTL 강제가 없어 죽은 에이전트가 이슈를 영구히 점유할 수 있습니다.
3. 도구 이름이 능력을 과장합니다 (`orchestrate`, `consensus`, `scale`, `gossip/crdt`). 정직한 note는 응답 안에 있고 README 쪽엔 없습니다.
4. JSON 파일 장부는 락이 있어도 다중 프로세스 쓰기와 크래시에서 정합성이 약합니다. event sourcing이 아닌 최신값 덮어쓰기입니다.
5. 기본값은 대부분 permissive입니다. 정책 강제와 인젝션 스크린이 모두 opt-in이고, 로컬 stdio 호출자는 인증이 없습니다.

## Kaisa에 가져올 아이디어와 피해야 할 점

**가져올 것**
- **"장부 ≠ 실행기" 분리를 이름까지 지키기**: Ruflo가 코드 주석으로 뒤늦게 인정한 구분입니다. Kaisa는 실행 주체(프로세스 supervisor)와 기록(ledger)을 처음부터 별개 모듈로 두고, 도구 이름도 `record`와 `run`을 구분하세요.
- **자식 프로세스 관리 패턴**: process group 단위 kill, stdin으로 프롬프트 전달, 출력 바이트 상한, worker별 타임아웃, 전역 `maxConcurrent` (`headless-worker-executor.ts`, `orchestrator.ts`). Rust에서는 이를 supervisor의 핵심 계약으로 삼으면 됩니다.
- **writer 제한과 worktree 격리**: `maxWriters`와 `worktreePath`를 코드로 강제하는 설계 (`orchestrator.ts:92-110,240`). 한 worktree에 writer 하나라는 규칙을 문서가 아니라 스케줄러 불변식으로 만드세요.
- **중앙 관문 하나**: 모든 도구 호출이 `callMCPTool` 한 곳을 지나며 정책 결정과 receipt를 남기는 구조 (`mcp-client.ts:255-296`). 정책 강제 시 fail-closed, 호출 예산은 롤링 윈도우라는 선택도 좋습니다.
- **합의 투표의 최소 규칙**: 등록된 투표자만 허용(Sybil 방지), 중복 투표 거부, term당 제안 1개, 필요 표수 계산 (`hive-mind-tools.ts:880-945`). 단 이것을 "BFT"라고 부르지는 마세요.
- **스키마 노출 최소화**: `tools/list` 필터링과 스키마 토큰 오버헤드 측정 (`mcp-server.ts:353-390`). 도구 수가 늘면 컨텍스트 비용이 됩니다.
- **orphan 재조정**: PID liveness(`kill(pid,0)`)와 TTL의 이중 방식 (`swarm-tools.ts:94-130`).

**피할 것**
- 레코드 개수를 "에이전트 100+"로 표현하지 마세요. 규모 지표는 실제 동시 프로세스 수와 처리량으로 정의하세요.
- 강제하지 않는 필드를 스키마에 두지 마세요 (`topology`, `ttlMs`, `maxAgents`, `communicationProtocol`, gossip/crdt). 쓰지 않을 값은 아예 노출하지 않거나, 강제하는 경로가 생길 때까지 거부하세요.
- lease는 만료와 갱신(heartbeat)을 포함해 만드세요. 만료 없는 claim은 죽은 에이전트에게 작업이 영구 잠기는 결함입니다.
- 정책, 인젝션 스크린, 인증을 opt-in 기본 off로 두지 마세요. 로컬 stdio에도 최소한 호출자 구분과 감사 로그는 기본으로 두세요.
- JSON 최신값 덮어쓰기 대신 append-only 이벤트 로그와 스냅샷을 쓰세요. 그래야 크래시 복구와 receipt 검증이 됩니다.
- 메시징을 호스트(SendMessage 같은 외부 기능)에 암묵적으로 떠넘기지 마세요. 에이전트 간 통신이 필요하면 Kaisa가 소유한 채널과 소유권 규칙을 정의하세요.
- 같은 이름의 두 구현이 공존하는 상태(`swarm` 패키지의 실제 코디네이터와 MCP 도구의 장부)를 만들지 마세요. 한 개념에는 하나의 구현만 두세요.
