# Finupdate Implementation Plan

This plan outlines the specific improvements to be implemented in the `finupdate` project:

## 1. Code Cleanup & Refactoring
- **Task 1.1:** [x] Remove obsolete comment forwarders in `src/ui/rebase_dialog.rs` to keep the UI module completely clean.

## 2. Test Improvements & Integration Testing Mocking (Strategy B)
- **Task 2.1:** [x] Implement robust mock-subprocess integration tests inside `src/orchestrator.rs` targeting the output scanner and the stream event loop. 
- **Task 2.2:** [x] Verify that `parse_line` and event propagation correctly process start, completion, failure, and output events in a simulated process run.

## 3. Code Coverage Setup (Strategy D)
- **Task 3.1:** [x] Integrate `coverage` recipe in the project's `justfile` using `cargo-llvm-cov` to allow developers to generate block coverage reports locally.

## 4. Architecture Improvements
- **Task 4.1:** [x] Implement an in-memory/on-disk caching layer for OCI Registry queries within `src/registry_client.rs` to optimize repeat queries when interacting with rebase and changelog pages.

## 5. CI/CD Integration
- **Task 5.1:** [x] Establish a comprehensive GitHub Actions CI pipeline in `.github/workflows/ci.yml` that automatically installs system dependencies (`libadwaita`, `gtk4`), runs formatting/clippy checks, executes all unit & integration tests, and generates code coverage metrics.

## 6. Real Shell Script & System Call Testing
- **Task 6.1:** [x] Implement `FINUPDATE_TEST_BREW_BIN` testing hook in `data/finupdate-runner` to support mock-executable paths.
- **Task 6.2:** [x] Establish comprehensive system command mocking (for `bootc`, `flatpak`, `su`/`brew`, `distrobox`) using custom mock binary creation in isolated test paths.
- **Task 6.3:** [x] Add real-script execution test cases (`test_real_runner_full_success_with_mocks` and `test_real_runner_system_only_skips_others`) executing `data/finupdate-runner` directly and asserting that correct system calls, parameters, and events are processed under full success and system-only modes.
- **Task 6.4:** [x] Implement mock systemctl and pkexec commands to cover timer installation, status, and control logic in `src/uupd_compat.rs` unit tests, driving code coverage up from 55% to over 80%.

