# Architecture Guidelines

## Project Structure

The crosstache project follows a modular Rust architecture with clear separation of concerns:

```
src/
├── auth/          # Authentication providers and Azure credential management
├── cli/           # Command-line interface and argument parsing
├── config/        # Configuration management and persistence
├── secret/        # Secret operations and management
├── utils/         # Utility functions and helpers
├── vault/         # Vault operations and management
├── error.rs       # Centralized error handling
├── lib.rs         # Library exports
└── main.rs        # Application entry point
```

## Key Architectural Decisions

### Hybrid Azure Integration
- **Authentication**: Uses Azure SDK v0.20 for credential management via `DefaultAzureCredential`
- **Operations**: Direct REST API calls to Azure Key Vault API v7.4 for full control
- **Rationale**: Azure SDK v0.20 has limitations with tag support; REST API ensures complete functionality

### Error Handling
- Centralized error types in `error.rs` using `thiserror`
- User-friendly error messages with actionable guidance
- Structured error variants for different failure scenarios

### Configuration Management
- Hierarchical configuration: CLI flags > Environment variables > Config file > Defaults
- Persistent configuration in `~/.config/xv/xv.conf`
- Context-aware vault operations

## Design Patterns

### Manager Pattern
- `SecretManager` for secret operations
- `VaultManager` for vault operations
- Consistent interface across all managers

### Provider Pattern
- `AuthProvider` trait for authentication abstraction
- `DefaultAzureCredentialProvider` implementation
- Enables testing and future auth method extensions