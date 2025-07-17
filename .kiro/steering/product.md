# Product Overview

crosstache is a comprehensive command-line tool for managing Azure Key Vaults, written in Rust. The binary is named `xv` and provides simplified access to secrets, vault management capabilities, and advanced features.

## Core Features

- **Secret Management**: Full CRUD operations for secrets with group organization and name sanitization
- **Vault Operations**: Create, delete, restore, and manage Azure Key Vaults
- **Access Control**: RBAC-based access management for users and service principals  
- **Import/Export**: Bulk secret operations with JSON/TXT/ENV formats
- **Configuration**: Persistent settings with environment variable overrides
- **Connection String Parsing**: Parse and display connection string components

## Key Differentiators

- **Name Sanitization**: Supports any secret name through automatic sanitization while preserving original names in tags
- **Group Organization**: Logical organization of secrets using tags and hierarchical grouping
- **Hybrid Azure Integration**: Uses Azure SDK for authentication but direct REST API calls for enhanced functionality
- **Cross-platform**: Supports Windows, macOS (Intel/Apple Silicon), and Linux

## Target Users

Developers and DevOps engineers who need reliable, efficient Azure Key Vault management from the command line.