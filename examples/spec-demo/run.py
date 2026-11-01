#!/usr/bin/env python3
"""Offline production-path demo. Only kaisa-fake-agent is launched."""
import argparse
import json
import os
from pathlib import Path
import re
import shutil
import sqlite3
import subprocess
import tempfile
import threading

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=ROOT / 'target/debug/kaisa')
    parser.add_argument('--fake', type=Path, default=ROOT / 'target/debug/kaisa-fake-agent')
    parser.add_argument('--measure-agent', action='store_true')
    parser.add_argument('--attack', choices=['exit-zero', 'cargo-runner', 'build-rs-private-swap'])
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    binary, fake, out = args.binary.resolve(), args.fake.resolve(), args.output.resolve()
    assert fake.name.startswith('kaisa-fake-agent'), 'this demo only permits the fake agent'
    out.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='kaisa-spec-demo-') as temp:
        temp = Path(temp)
        repo, state = temp / 'repo', temp / 'state'
        stop = threading.Event()
        if args.measure_agent:
            import shlex
            wrapper = temp / 'kaisa-fake-agent-timed'
            wrapper.write_text('#!/bin/sh\nif [ "$1" = "--mcp-config" ]; then\n stats="${2%/*}/scratch/kaisa-fake-time.txt"\nelse\n stats="$TMPDIR/kaisa-fake-time.txt"\nfi\nexec /usr/bin/time -l -o "$stats" ' + shlex.quote(str(fake)) + ' "$@"\n')
            wrapper.chmod(0o755)
            fake = wrapper
            timing_out = out / 'agent-times'
            timing_out.mkdir(exist_ok=True)
            def collect_times():
                while not stop.is_set():
                    paths = list(state.glob('attempts/*/scratch/kaisa-fake-time.txt'))
                    paths += list(state.glob('checks/*/author_*/scratch/kaisa-fake-time.txt'))
                    for path in paths:
                        try:
                            value = path.read_text()
                            if 'maximum resident set size' in value:
                                identity = path.parent.parent.name.replace('author_', 'att_')
                                (timing_out / (identity + '.time.txt')).write_text(value)
                        except FileNotFoundError:
                            pass
                    stop.wait(0.005)
            collector = threading.Thread(target=collect_times, daemon=True)
            collector.start()
        shutil.copytree(HERE / 'repo', repo)
        env = dict(os.environ, KAISA_STATE_DIR=str(state), CARGO_NET_OFFLINE='true')
        env.pop('KAISA_ATTEMPT', None)
        def command(stage, argv, expected=0):
            timing = out / (stage + '.time.txt')
            cmd = ['/usr/bin/time', '-l', '-o', str(timing)] + list(map(str, argv)) if os.uname().sysname == 'Darwin' else list(map(str, argv))
            result = subprocess.run(cmd, cwd=repo, env=env, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
            (out / (stage + '.txt')).write_text(result.stdout)
            assert result.returncode == expected, (stage, result.returncode, result.stdout)
            return result.stdout
        for argv in [['git', 'init', '-q'], ['git', 'add', '.'], ['git', '-c', 'user.name=Demo', '-c', 'user.email=demo@example.invalid', 'commit', '-qm', 'legacy base']]:
            subprocess.run(argv, cwd=repo, check=True)
        proposal = dict(title='Reject empty inputs', criteria=[], ambiguities=[], invariants=[], error_behaviour=[])
        proposal_path = temp / 'proposal.json'
        def draft(stage, identity):
            proposal_path.write_text(json.dumps(proposal))
            return command(stage, [binary, 'spec', 'draft', '--file', HERE / 'requirements.txt', '--id', identity, '--agent-program', fake, '--agent-arg', '--fake-spec', '--agent-arg', proposal_path])
        assert 'missing acceptance criteria: R1' in draft('01-missing-criteria', 'spec_missing')
        proposal['criteria'] = [dict(id='AC-1', requirement_ids=['R1'], given='empty input', when='accept is called', then='false', kind='Normal', correction=True)]
        draft('02-draft', 'spec_demo')
        command('03-show', [binary, 'spec', 'show', 'spec_demo'])
        command('04-revision-policy', [binary, 'policy', 'set', 'revision_limit', '0'])
        command('04-policy', [binary, 'policy', 'set', 'allow_provisional_spec', 'true'])
        assert 'NOT human-approved' in command('05-approve', [binary, 'spec', 'approve', 'spec_demo', '--provisional'])
        qa = temp / 'qa.json'
        qa.write_text(json.dumps({'tests': [dict(test_id='empty-input', criterion_ids=['AC-1'], file='empty.rs', contents='#[test] fn empty_input_is_rejected() { assert_eq!(spec_fixture::accept(""), false); }')]}))
        characterization = temp / 'characterization.json'
        characterization.write_text(json.dumps(dict(tests=[dict(test_id='legacy-empty', file='legacy.rs', contents='#[test] fn legacy_empty() { assert!(spec_fixture::accept("")); }')], behaviours=[dict(test_id='legacy-empty', path='src/lib.rs', description='empty input currently accepted', edge_case=True, disposition=dict(kind='correct', criterion_id='AC-1'))])))
        domain = temp / 'domain.txt'
        domain.write_text('```kaisa-test-review\n' + json.dumps(dict(criteria=[dict(id='AC-1', coverage='covered', note='contract asserted')], missing_business_cases=[], missing_state_transitions=[], weak_assertions=[], spec_test_mismatches=[])) + '\n```')
        reports = []
        for mode, expression, expected in ([('fail', 'true', 4)] if args.attack else [('pass', '!input.is_empty()', 0), ('fail', 'true', 4)]):
            content = 'pub fn accept(input: &str) -> bool { ' + expression + ' }\n#[cfg(test)] mod tests { #[test] fn nonempty() { assert!(super::accept("value")); } }\n'
            write_path = 'src/lib.rs'
            if args.attack == 'exit-zero':
                content = content.replace('true }', 'std::process::exit(0) }')
            elif args.attack == 'cargo-runner':
                write_path, content = '.cargo/config.toml', '[target.\'cfg(all())\']\nrunner = \"/usr/bin/true\"\n'
            elif args.attack == 'build-rs-private-swap':
                write_path = 'build.rs'
                content = 'fn main() { let manifest = std::fs::read_to_string("Cargo.toml").unwrap(); for line in manifest.lines().filter(|line| line.starts_with("path = ")) { let path = line.trim_start_matches("path = ").trim_matches(\'"\'); if path.contains("kaisa_qa_") { std::fs::write(path, "#[test] fn empty_input_is_rejected() {}").unwrap(); } } }\n'
            build = [binary, 'build', '--spec', 'spec_demo', '--agent-program', fake, '--agent-arg', '--fake-write', '--agent-arg', write_path, '--agent-arg', '--fake-content', '--agent-arg', content, '--reviewer-arg', '--fake-staged-review', '--reviewer-arg', 'resolve', '--qa-author-arg', '--fake-qa', '--qa-author-arg', qa, '--qa-author-arg', '--fake-characterization-answer', '--qa-author-arg', characterization, '--qa-blind-arg', '--fake-qa', '--qa-blind-arg', qa, '--test-reviewer-arg', '--fake-test-review-answer', '--test-reviewer-arg', domain, '--check-program', shutil.which('cargo'), '--check-arg', 'test', '--check-arg', '--offline']
            # macOS has a labelled descendant-containment reduction; other hosts need explicit acceptance.
            if os.uname().sysname != 'Darwin':
                build += ['--accept-qa-isolation', 'reduced(offline demo host)']
            output = command('06-build-' + mode, build, expected)
            assert ('PASS' if mode == 'pass' else 'FAIL') in output, output
            run = re.search(r'^Run: (\S+)', output, re.M).group(1)
            report = command('07-report-' + mode, [binary, 'spec', 'report', run])
            assert 'AC-1' in report and 'R1' in report
            if not args.attack:
                assert 'Pass: staged review approved' in report
                assert 'Pass: domain test review no blocking' in report
                assert ('Result: kaisa/result/' in report) == (mode == 'pass')
                if mode == 'fail':
                    assert 'Fail: private QA all passed on FINAL tree' in report
            reports.append('\n## ' + mode.upper() + '\n\n' + report)
            trace = json.loads((state / 'spec-runs' / run / 'traceability.json').read_text())
            (out / ('traceability-' + mode + '.json')).write_text(json.dumps(trace, indent=2) + '\n')
            database = sqlite3.connect(state / 'kaisa.db')
            columns = [row[1] for row in database.execute('pragma table_info(events)')]
            rows = [dict(zip(columns, row)) for row in database.execute('select * from events')]
            database.close()
            (out / ('ledger-' + mode + '.json')).write_text(json.dumps(rows, indent=2, default=lambda value: value.hex()) + '\n')
        stop.set()
        if args.measure_agent:
            collector.join()
        (out / 'traceability.md').write_text('# Recorded CLI spec reports\n' + ''.join(reports))
        (out / 'traceability.json').write_text(json.dumps({mode: json.loads((out / ('traceability-' + mode + '.json')).read_text()) for mode in (['fail'] if args.attack else ['pass', 'fail'])}, indent=2) + '\n')
    print('offline spec demo: attack blocked' if args.attack else 'offline spec demo: PASS candidate delivered; FAIL candidate blocked')


if __name__ == '__main__':
    main()
