# Contributing to Nexus

Thank you for your interest in contributing to Nexus. This document provides guidelines and instructions for contributing to the project.

## Development Setup

### Prerequisites

- Rust 1.75 or later
- Cargo (included with Rust)
- Git

### Clone and Build

```bash
git clone https://github.com/Adaptive-Liquidity/Nexus.git
cd Nexus/nexus
cargo build --release
```

### Running Tests

```bash
# Run all tests
cargo test

# Run with verbose output
cargo test --verbose

# Run specific test
cargo test test_name
```

### Code Formatting

Nexus uses `rustfmt` for code formatting. Please ensure your code is formatted before submitting:

```bash
cargo fmt --check
cargo fmt
```

### Linting

We use `clippy` for linting. Please ensure there are no warnings:

```bash
cargo clippy --all-targets -- -D warnings
```

## Pull Request Process

### 1. Create a Feature Branch

```bash
git checkout -b feature/your-feature-name
```

### 2. Make Your Changes

- Write code following the existing style
- Add tests for new functionality
- Update documentation as needed
- Ensure all tests pass

### 3. Commit Your Changes

```bash
git add .
git commit -m "Description of your changes"
```

Commit messages should follow the conventional commits format:

- `feat:` for new features
- `fix:` for bug fixes
- `docs:` for documentation changes
- `test:` for test changes
- `refactor:` for code refactoring

### 4. Push and Create PR

```bash
git push origin feature/your-feature-name
```

Then create a pull request on GitHub with a clear description of the changes.

## Code Style Guidelines

### Rust Conventions

1. **Naming**: Use descriptive names. Variables and functions should clearly indicate their purpose.
2. **Documentation**: Public APIs should have doc comments explaining their behavior.
3. **Error Handling**: Use `Result` types for fallible operations. Avoid `unwrap()` in production code.
4. **Testing**: Write tests for all public functions and important internal logic.

### Documentation Conventions

1. **Clarity**: Write clear, concise documentation.
2. **Examples**: Include examples for complex functionality.
3. **Updates**: Keep documentation in sync with code changes.

## Reporting Issues

### Bug Reports

Please include:

- Clear description of the bug
- Steps to reproduce
- Expected behavior
- Actual behavior
- Environment (OS, Rust version, etc.)

### Feature Requests

Please include:

- Clear description of the feature
- Use case / motivation
- Potential implementation approach (if applicable)

## Project Structure

```
nexus/
├── src/
│   ├── main.rs           # CLI entry point
│   ├── lib.rs            # Public API exports
│   ├── hypervisor/       # Core orchestration logic
│   ├── sandbox/          # WASM execution engine
│   ├── snapshot/         # State management
│   ├── telemetry/         # AI telemetry
│   ├── security/         # Access control
│   └── error.rs          # Error types
├── tests/                # Integration tests
├── Cargo.toml           # Dependencies
└── README.md            # Project documentation
```

## Testing Guidelines

### Unit Tests

- Test individual functions and methods
- Mock external dependencies
- Cover happy path and error cases

### Integration Tests

- Test component interactions
- Test end-to-end workflows
- Test CLI functionality

### Benchmark Tests

- Run benchmarks with `--release` flag
- Use consistent iteration counts
- Report both absolute and relative results

## Contact

For questions or concerns, please open an issue on GitHub or reach out to the maintainers.

## License

By contributing to Nexus, you agree that your contributions will be licensed under the MIT License.