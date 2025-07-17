# Coding Standards

## Rust Code Style

### General Principles
- Follow Rust idioms and best practices
- Use `rustfmt` for consistent formatting
- Run `clippy` for linting and suggestions
- Prefer explicit error handling over panics
- Use meaningful variable and function names

### Error Handling
- Use the centralized `crosstacheError` enum from `error.rs`
- Return `Result<T>` for fallible operations
- Provide user-friendly error messages with actionable guidance
- Use `?` operator for error propagation
- Create specific error variants for different failure scenarios

### Async Programming
- Use `tokio` as the async runtime
- Prefer `async/await` over manual Future implementations
- Use `Arc` for shared state across async boundaries
- Handle cancellation gracefully where appropriate

### Dependencies
- Use stable, well-maintained crates
- Pin major versions in `Cargo.toml`
- Prefer `rustls` over OpenSSL for TLS
- Use `serde` for serialization with derive macros

### Documentation
- Document all public APIs with `///` comments
- Include examples in documentation where helpful
- Use `//!` for module-level documentation
- Keep comments concise and focused on "why" not "what"

## Code Organization

### Module Structure
- One primary concern per module
- Use `mod.rs` files to organize module exports
- Keep modules focused and cohesive
- Prefer composition over inheritance

### Naming Conventions
- Use `snake_case` for functions, variables, and modules
- Use `PascalCase` for types, structs, and enums
- Use `SCREAMING_SNAKE_CASE` for constants
- Prefix private items with underscore when needed for clarity

### Testing
- Write unit tests in the same file using `#[cfg(test)]`
- Use integration tests in the `tests/` directory
- Mock external dependencies using `mockall`
- Test error conditions and edge cases