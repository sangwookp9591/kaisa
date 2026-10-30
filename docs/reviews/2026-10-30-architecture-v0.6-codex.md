# Kaisa v0.6 설계 검토

검토일: 2026-10-30. `architecture.md` v0.6(요청 절 전체), `prd.md` v1.1, `user-flows.md` v0.3, `research/ruflo-control.md`와 근거 보고서 a/b/c를 정적으로 교차 검토했다. 아래 `A`, `P`, `F`는 각각 architecture 절, PRD 절, user flow 식별자다. 구현·벤치마크를 실행하지 않았으므로 “강제”는 설계상 실행 경계가 명시되었다는 뜻이며 구현 검증을 뜻하지 않는다.

전반적으로 context assembly, router spawn, immutable verification, daemon delivery gate가 실행 경로에 놓였다. 그러나 **쓰기·외부 효과·최종 통합 검증의 경계를 닫기 전에는 “검증된 결과만 전달”을 보장할 수 없다.** 발견사항은 blocker 3건, major 12건, minor 1건이다.

## 여덟 요구의 판정

| 요구 | 판정과 근거 |
|---|---|
| 1. 역할 팀·scoped context | 부분 강제. 역할별 도구·context pack은 Kaisa가 선택한다(A5.2/5.9). 전체 저장소 읽기·추가 도구 출력·세션 길이까지 제한하지 않아 context collapse 방지는 미완성이다. |
| 2. 자동 교차 검증 | 강제 설계. checks/review/revision/delivery는 상태기계다(A5.8). security는 조건부이며 최종 통합·비쓰기 답변에는 빈틈이 있다. |
| 3. 세션 간 프로젝트 기억 | 강제 설계. ledger, profile, 필수 pack 주입이 있어 에이전트의 자발적 검색에 의존하지 않는다(A5.9). 무효화·삭제 계약은 부족하다. |
| 4. 비용·모델 routing | 모델 선택은 강제(A5.10). 비용 hard cap은 optional gateway 없이는 추정 예약과 사후 사용량에 의존한다. |
| 5. 100+ 전문 swarm | hierarchy·slots는 강제 설계(A5.13). 실제 agent CLI·검증·provider 제한을 포함한 100+ 처리량은 미입증이다. |
| 6. 자기학습·vector RAG | 자동 갱신·검색 경로는 설계됐다(A5.9/5.10). vector는 evidence gate 이후이며 BM25 fallback이 허용된다. 유용성과 안전한 학습은 아직 보장되지 않는다. |
| 7. MCP·Claude/Codex/Hermes | broker chokepoint는 강제 설계(A5.6). adapter 지원은 A19.1의 미해결 전제이고 Hermes/MCP relay는 M2다. |
| 8. TUI·web 실시간 관측 | event projection으로 설계됨(A5.11/12). TUI M1, console M2이며 stream 재연결·정지·권한 계약 보완이 필요하다. |

## 발견사항과 수정안

### B1 — blocker: `exec`가 write set을 우회할 수 있다

**위치:** A5.2, A5.6–5.7. `fs.write` 거부와 달리 `exec`의 sandbox는 worktree 전체 쓰기를 허용한다. `python`이나 빌드 스크립트가 out-of-set 파일을 바꾸면 broker의 경로 검사를 거치지 않는다. **수정:** 모든 자식 프로세스에 동일한 write-set filesystem 권한을 강제하고 빌드 산출물 전용 writable 영역을 분리한다. symlink/hardlink/rename, shell·임의 interpreter·MCP 경유 쓰기를 negative corpus에 넣는다. diff 사후 검사는 전달 차단의 보조 수단으로 둔다.

### B2 — blocker: MCP relay는 외부 효과의 우회 출구다

**위치:** A5.6, A5.7, A5.8, A5.11; P FR-32. 등록된 서버의 tool policy만으로 DB 변경·브라우저 클릭·배포가 delivery/Decision 경계를 지킨다는 계약이 없다. 서버 프로세스는 주입받은 credential과 자체 filesystem/network 권한을 갖는다. **수정:** 서버도 sandbox·egress·credential scope로 격리하고 tool/schema/version을 고정한다. 효과 분류별 capability·preview·outbox·reconciliation을 의무화하며 unknown effect는 거부한다. tool output과 resource도 untrusted로 취급하고 크기·시간·sampling/elicitation·중첩 호출을 제한한다.

### B3 — blocker: 최종 통합 결과의 review/security가 빠질 수 있다

**위치:** A5.8, A5.13. 개별 Task는 전체 loop를 거치지만 sub-run 통합은 명시적으로 `Checks`만 요구한다. 서로 통과한 변경의 조합이나 conflict revision은 새로운 취약점을 만들 수 있다. **수정:** 각 merge/conflict 결과와 최종 Mission source digest에 전체 필수 checks/review/security를 다시 결합한다. 최종 diff에서 security applicability를 재계산하고 자식 receipt의 단순 합산을 금지한다.

### M1 — major: quoted knowledge는 prompt injection 방벽이 아니다

**위치:** A2 P15/P19, A5.8–5.9. 통과한 코드가 lesson 문장까지 증명하지 않으며 “무엇을 읽는지만 바꾼다”는 영향 제한이 아니다. 악성 note를 사용한 작업 하나가 통과하면 신뢰가 세탁될 수 있다. coordinator 기록도 daemon 저장만으로 trusted가 된다. **수정:** 데이터 provenance와 사실 검증을 분리하고 문장별 근거·revision scope를 요구한다. imported/agent-authored 항목은 quarantine하며 명령·권한 요청을 승격하지 않는다. injection·compaction·handoff 전체에 taint를 유지하고 adversarial retrieval 테스트를 추가한다.

### M2 — major: 학습 보상을 게임할 수 있다

**위치:** A5.9 accountability, A5.10 priors, A7. 모든 injected item에 같은 최종 성패를 주면 무관한 항목이 보상을 얻고 실패 원인은 잘못 귀속된다. 쉬운 Task 대량 분할·반복 실행은 특정 option의 prior를 장악한다. revision 비용과 지연 발견 결함의 귀속도 없다. **수정:** held-out benchmark·shadow 평가, 선택 편향 보정, 업데이트 상한·중복 제거·최소 표본을 정의한다. 원 attempt와 revision 총비용을 묶고 override/Tampered는 positive reward에서 제외한다. model/prompt/verifier version별 prior와 회귀 시 rollback을 저장한다.

### M3 — major: hard budget의 강제 경계가 선택 사항이다

**위치:** A5.10, A9, A17; P G9/FR-19; F19j/F23. 동시 요청의 비용이 예약 추정치를 넘으면 100% 정지는 이미 늦다. subscription/turn-only adapter는 요청 전 차단을 증명하지 못한다. **수정:** Mission–sub-run–user–team 예약을 원자적으로 수행하고 request별 최대 출력·비용 상한을 중개한다. hard-cap 지원이 없는 adapter는 별도 capability와 초과 가능 한도를 표시한다. gateway 없이도 동일 보장이 있다는 milestone 표현을 수정한다.

### M4 — major: loopback 보호는 proxy 우회를 포함해야 한다

**위치:** A5.7, A5.11 console, A16; F10. 직접 loopback을 막아도 broker proxy/MCP가 loopback으로 연결하면 console에 도달할 수 있다. 랜덤 port와 Origin만으로 이 경로가 닫히지 않는다. **수정:** DNS 해석 후 매 연결·redirect의 loopback/private/link-local 목적지를 차단하고 console/control을 명시적으로 제외한다. exact Host+port/Origin, CSRF, 토큰 교환·만료·재사용 거부, tab 종료 heartbeat/서버 TTL을 정의한다. transcript는 HTML/ANSI escape를 정화하고 SSE 재연결·중복·slow consumer를 제한한다. same-user 악성 프로세스의 잔여 위험도 명시한다.

### M5 — major: local endpoint가 안전하거나 저렴하다는 근거가 없다

**위치:** A5.6, A5.10, A14; F37. catalog는 local server와 company gateway를 같은 종류로 취급하며 endpoint 신원·redirect·모델 변경·데이터 반출 규칙이 없다. 악성 서버는 diff를 수집하거나 usage를 위조할 수 있다. **수정:** endpoint/모델 digest·data classification·TLS 또는 로컬 peer identity를 고정하고 임의 URL을 거부한다. timeout·request/response/token/CPU·동시성 상한과 장애 격리를 둔다. embedding 자산은 신뢰된 manifest에서 checksum을 얻고 native inference 장애도 격리한다.

### M6 — major: context pack에 overflow·freshness 규칙이 없다

**위치:** A2 P18, A5.2, A5.9, A7. spec/acceptance가 예산보다 크거나 scanner가 동적 의존성을 놓치는 경우가 없다. 후속 read/tool output과 장기 세션이 다시 context를 무한히 키울 수 있다. **수정:** 필수 정보는 truncate하지 말고 Task 분할로 처리하며 role별 읽기·출력·누적 context 예산을 집행한다. unknown impact는 보수적으로 검증 범위를 넓힌다. profile/index/knowledge에 base·toolchain·model version과 무효화 규칙을 둔다.

### M7 — major: 검증 계약과 화면이 충돌한다

**위치:** A5.8; F03/F06/F09/F16. F06은 `2 major` 후보를 선택 가능하게 보여 주고 F03은 checks 없이 unverified 결과를 허용한다. F09 같은 비쓰기 답변에는 검증 loop가 없다. **수정:** finding severity로 eligibility를 단일 계산하고 모든 화면에 공유한다. unverified는 별도 비승인 상태로 두며 override receipt를 verified와 구별한다. 답변에도 주장·인용 검증과 독립 review를 요구하고 raw terminal/progress와 delivered result를 명확히 구분한다.

### M8 — major: verification 실행·예외 경로가 불완전하다

**위치:** A5.8, A5.10, A16. read-only checkout에서 빌드 산출물을 만드는 방법, legitimate test 수정의 appeal, malformed verdict 반복·reviewer 불능·flake·timeout이 빠졌다. tool-less local review는 필수 `kaisa_report`를 호출할 수도 없다. **수정:** immutable source와 writable ephemeral build 영역을 분리한다. base/candidate 비교 실행과 test 변경 전용 검토를 정의하고 예외는 override로 남긴다. failure taxonomy·재시도 상한·reviewer fallback 및 local JSON 응답을 daemon verdict로 변환하는 검증 adapter를 명시한다.

### M9 — major: 100 scripted agents는 실제 100-agent 서비스 검증이 아니다

**위치:** A5.2/5.13/15/16; P G10. benchmark는 agent CLI 메모리, 모델 rate limit, review/security 증폭, 빌드·checkout·embedding 부하를 대표하지 않는다. shared slot keeper와 per-attempt keeper(A5.2/A7/ADR-0013)도 일치하지 않는다. **수정:** reference host·역할 혼합·측정 지속시간·총 RSS/FD/디스크·완료 처리량·p95 queue latency를 고정한다. 실제 CLI conformance 부하와 scripted chaos를 분리하고 cross-sub-run 경로 소유권을 Mission 전역에서 원자적으로 확보한다. 계층 교착·starvation·writer saturation 시 backpressure와 keeper 장애 범위를 검증한다.

### M10 — major: “처음부터 쉬움”과 milestone이 맞지 않는다

**위치:** A17/19.1; P §8/§9; F02/F03/F25/F37/§9–10. M0는 첫 verified 결과를 요구하지만 verify는 M1이다. M1 local model 등록은 M3 admin flow에 기대며 brokered 지원 자체도 미확정이다. **수정:** M0 first-result 기준을 정직하게 낮추거나 최소 verify를 당긴다. 필요한 agent 등록·개인 기본 정책은 M1에 제공하고 세 OS brokered conformance를 구현 착수 전 go/no-go로 둔다. 첫 실행에 설치·로그인·모델 다운로드·검증 시간을 포함해 10분 UX를 평가한다.

### M11 — major: authority와 등급 정의가 서로 다르다

**위치:** A5.2/5.10/5.12; F07/U9. security 기본은 T2지만 grade 표는 T3, F07은 review까지 항상 strongest다. `self` 규칙은 “requester may approve”여서 requester bridge의 승인 금지와 충돌한다. **수정:** role→최소 grade 표 하나를 사용하고 final/security의 downgrade 금지를 schema로 검사한다. `self`는 authenticated initiating human으로 정의하고 Requester token은 항상 승인 불가로 고정한다.

### M12 — major: 만료·삭제·재시작 시 안전한 상태가 빠졌다

**위치:** A5.9/5.12/A7/A16; F19n/F21. Decision은 다음 snapshot에서만 빠지고 running reduced-protection process를 어떻게 막는지 없다. memory `forget`도 event log·vector·context cache에 남을 수 있으며 event/vector 저장 간 crash 일관성이 불명확하다. **수정:** expiry를 dispatch/effect 시점마다 재검사하고 만료 시 freeze/revoke한다. knowledge tombstone·재생 시 부활 금지·원본/파생물 삭제 정책과 rebuildable vector watermark를 정의한다. sleep/clock skew·disk full·index 손상 복구를 시험한다.

### N1 — minor: 계약 표와 기본 출력의 drift

**위치:** P §9, A6/A13/A17, F04/F26/§9. P는 vector를 M3까지 off라 하지만 A는 M1이다. Linear도 P는 v1 제외, A13/F08은 M2, flow mapping은 M3다. F26에 exit 6이 없고 F04의 `T1`은 U5의 tier 비노출과 충돌한다. **수정:** capability/milestone/exit schema를 단일 원본으로 생성하고 기본 출력은 “가벼운 모델”로 표시한다.

## 우선순위

B1–B3의 실행 불변식과 negative test를 먼저 확정하고, M1–M5의 학습·비용·네트워크 신뢰 경계를 닫는다. 이후 실제 역할 혼합 부하와 세 OS 신규 사용자 시나리오로 검증한다. ruflo 근거 a/b/c는 비교 설계의 근거이지 Kaisa 구현 증거가 아니므로, 이를 Kaisa의 shipped/enforced 증명으로 재사용해서는 안 된다.
