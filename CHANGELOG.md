# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Builder API for Jobs
- Scheduling jobs to execute after some point in the future
- Prioritization of jobs.
- Postgresql support

### Fixes

- Tasks scheduled in the future will eventually be executed.

### Breaking Changes

- `run_job_loop` and the `Never` type got removed.
- sqlite paths now require to be prepended with `sqlite://`

### Administrator Note

This release contains a database schema change. It may take longer than usual to start the queue up.

## [0.1.0] - 2024-05-05

[unreleased]: https://github.com/DarkKirb/app-queue/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/DarkKirb/app-queue/releases/tag/v0.1.0
