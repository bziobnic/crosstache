# Product Overview

crosstache is a cross-platform secrets manager for the command line, written in Rust. The binary is named `xv`. Currently backed by Azure Key Vault, with plans to support additional backends (AWS Secrets Manager, HashiCorp Vault, etc.).

## Core Features

- **Secret Management**: Full CRUD with group organization, folders, notes, tags, and name sanitization
- **Secret Injection**: Run processes with secrets as env vars (`xv run`), render templates (`xv inject`)
- **Secret Lifecycle**: Version history, rollback, rotation, expiration, cross-vault copy/move
- **Vault Operations**: Create, delete, restore, and manage vaults
- **Environment Profiles**: Named profiles mapping to vaults/groups; `.env` file sync
- **File Storage**: Optional blob storage for files (behind `file-ops` feature flag)
- **Import/Export**: Bulk secret operations with JSON/TXT/ENV formats

## Key Differentiators

- **Name Sanitization**: Any secret name works — automatically sanitized, originals preserved in metadata
- **Group Organization**: Flexible tag-based grouping with merge/replace semantics
- **Backend-Agnostic Design**: Manager/provider patterns allow future backend additions
- **Security First**: Zeroized memory, restricted file permissions, clipboard auto-clear, output masking

## Target Users

Developers and DevOps engineers who manage secrets across environments and want a clean, consistent CLI regardless of backend.
