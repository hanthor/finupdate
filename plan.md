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

