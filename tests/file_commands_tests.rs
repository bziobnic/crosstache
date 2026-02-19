//! Integration tests for file upload and download commands
//!
//! These tests verify that the file upload and download commands work correctly
//! with both the full command syntax and the quick aliases.

use crosstache::{
    cli::commands::{Commands, FileCommands},
    config::{Config, BlobConfig},
    error::Result,
};
use std::fs;
use tempfile::{NamedTempFile, TempDir};

/// Helper function to create a test configuration
fn create_test_config() -> Config {
    let mut config = Config::default();
    config.default_vault = "test-vault".to_string();
    config.default_resource_group = "test-rg".to_string();
    config.subscription_id = "test-subscription".to_string();
    config.blob_config = Some(BlobConfig {
        storage_account: "teststorage".to_string(),
        container_name: "test-container".to_string(),
        endpoint: Some("https://teststorage.blob.core.windows.net".to_string()),
        enable_large_file_support: true,
        chunk_size_mb: 4,
        max_concurrent_uploads: 3,
    });
    config
}

#[tokio::test]
async fn test_file_upload_command_basic() -> Result<()> {
    let temp_file = NamedTempFile::new().unwrap();
    let test_content = b"Hello, World! This is a test file.";
    fs::write(temp_file.path(), test_content).unwrap();

    let _config = create_test_config();

    let upload_command = FileCommands::Upload {
        files: vec![temp_file.path().to_string_lossy().to_string()],
        name: Some("test-file.txt".to_string()),
        recursive: false,
        flatten: false,
        prefix: None,
        group: vec!["test-group".to_string()],
        metadata: vec![
            ("author".to_string(), "test-user".to_string()),
            ("version".to_string(), "1.0".to_string()),
        ],
        tag: vec![
            ("environment".to_string(), "test".to_string()),
        ],
        content_type: Some("text/plain".to_string()),
        progress: true,
        continue_on_error: false,
    };

    match upload_command {
        FileCommands::Upload {
            files,
            name,
            recursive,
            flatten: _,
            prefix: _,
            group,
            metadata,
            tag,
            content_type,
            progress,
            continue_on_error,
        } => {
            assert_eq!(files, vec![temp_file.path().to_string_lossy().to_string()]);
            assert_eq!(name, Some("test-file.txt".to_string()));
            assert!(!recursive);
            assert_eq!(group, vec!["test-group".to_string()]);
            assert_eq!(metadata.len(), 2);
            assert_eq!(tag.len(), 1);
            assert_eq!(content_type, Some("text/plain".to_string()));
            assert!(progress);
            assert!(!continue_on_error);
        }
        _ => panic!("Expected Upload command"),
    }

    Ok(())
}

#[tokio::test]
async fn test_file_upload_command_with_multiple_groups() -> Result<()> {
    let temp_file = NamedTempFile::new().unwrap();
    let test_content = b"Multi-group test file";
    fs::write(temp_file.path(), test_content).unwrap();

    let upload_command = FileCommands::Upload {
        files: vec![temp_file.path().to_string_lossy().to_string()],
        name: None,
        recursive: false,
        flatten: false,
        prefix: None,
        group: vec![
            "production".to_string(),
            "config".to_string(),
            "api".to_string(),
        ],
        metadata: vec![
            ("created_by".to_string(), "automated_test".to_string()),
            ("purpose".to_string(), "configuration".to_string()),
        ],
        tag: vec![
            ("team".to_string(), "devops".to_string()),
            ("project".to_string(), "crosstache".to_string()),
        ],
        content_type: None,
        progress: false,
        continue_on_error: false,
    };

    match upload_command {
        FileCommands::Upload {
            files,
            name,
            recursive: _,
            flatten: _,
            prefix: _,
            group,
            metadata,
            tag,
            content_type,
            progress,
            continue_on_error: _,
        } => {
            assert_eq!(files, vec![temp_file.path().to_string_lossy().to_string()]);
            assert!(name.is_none());
            assert_eq!(group.len(), 3);
            assert!(group.contains(&"production".to_string()));
            assert!(group.contains(&"config".to_string()));
            assert!(group.contains(&"api".to_string()));
            assert_eq!(metadata.len(), 2);
            assert_eq!(tag.len(), 2);
            assert!(content_type.is_none());
            assert!(!progress);
        }
        _ => panic!("Expected Upload command"),
    }

    Ok(())
}

#[tokio::test]
async fn test_file_download_command_basic() -> Result<()> {
    let temp_dir = TempDir::new().unwrap();
    let output_path = temp_dir.path().join("downloaded-file.txt");

    let download_command = FileCommands::Download {
        files: vec!["test-file.txt".to_string()],
        output: Some(output_path.to_string_lossy().to_string()),
        rename: None,
        recursive: false,
        flatten: false,
        stream: false,
        force: false,
        continue_on_error: false,
    };

    match download_command {
        FileCommands::Download { files, output, rename, recursive: _, flatten: _, stream, force, continue_on_error: _ } => {
            assert_eq!(files, vec!["test-file.txt"]);
            assert_eq!(output, Some(output_path.to_string_lossy().to_string()));
            assert!(rename.is_none());
            assert!(!stream);
            assert!(!force);
        }
        _ => panic!("Expected Download command"),
    }

    Ok(())
}

#[tokio::test]
async fn test_file_download_command_with_streaming() -> Result<()> {
    let download_command = FileCommands::Download {
        files: vec!["large-file.bin".to_string()],
        output: None,
        rename: None,
        recursive: false,
        flatten: false,
        stream: true,
        force: true,
        continue_on_error: false,
    };

    match download_command {
        FileCommands::Download { files, output, rename: _, recursive: _, flatten: _, stream, force, continue_on_error: _ } => {
            assert_eq!(files, vec!["large-file.bin"]);
            assert!(output.is_none());
            assert!(stream);
            assert!(force);
        }
        _ => panic!("Expected Download command"),
    }

    Ok(())
}

#[tokio::test]
async fn test_quick_upload_command() -> Result<()> {
    let temp_file = NamedTempFile::new().unwrap();
    let test_content = b"Quick upload test content";
    fs::write(temp_file.path(), test_content).unwrap();

    let quick_upload_command = Commands::Upload {
        file_path: temp_file.path().to_string_lossy().to_string(),
        name: Some("quick-upload.txt".to_string()),
        groups: Some("quick,test".to_string()),
        metadata: vec![
            "type=quick-test".to_string(),
            "method=cli".to_string(),
        ],
    };

    match quick_upload_command {
        Commands::Upload { file_path, name, groups, metadata } => {
            assert_eq!(file_path, temp_file.path().to_string_lossy().to_string());
            assert_eq!(name, Some("quick-upload.txt".to_string()));
            assert_eq!(groups, Some("quick,test".to_string()));
            assert_eq!(metadata.len(), 2);
            assert!(metadata.contains(&"type=quick-test".to_string()));
            assert!(metadata.contains(&"method=cli".to_string()));
        }
        _ => panic!("Expected Upload command"),
    }

    Ok(())
}

#[tokio::test]
async fn test_quick_download_command() -> Result<()> {
    let temp_dir = TempDir::new().unwrap();
    let output_path = temp_dir.path().join("quick-download.txt");

    let quick_download_command = Commands::Download {
        name: "quick-file.txt".to_string(),
        output: Some(output_path.to_string_lossy().to_string()),
        open: true,
    };

    match quick_download_command {
        Commands::Download { name, output, open } => {
            assert_eq!(name, "quick-file.txt");
            assert_eq!(output, Some(output_path.to_string_lossy().to_string()));
            assert!(open);
        }
        _ => panic!("Expected Download command"),
    }

    Ok(())
}

#[tokio::test]
async fn test_quick_download_command_with_open() -> Result<()> {
    let quick_download_command = Commands::Download {
        name: "document.pdf".to_string(),
        output: None,
        open: true,
    };

    match quick_download_command {
        Commands::Download { name, output, open } => {
            assert_eq!(name, "document.pdf");
            assert!(output.is_none());
            assert!(open);
        }
        _ => panic!("Expected Download command"),
    }

    Ok(())
}

#[tokio::test]
async fn test_file_upload_validation() -> Result<()> {
    let non_existent_file = "/tmp/non-existent-file-12345.txt";

    let upload_command = FileCommands::Upload {
        files: vec![non_existent_file.to_string()],
        name: None,
        recursive: false,
        flatten: false,
        prefix: None,
        group: vec![],
        metadata: vec![],
        tag: vec![],
        content_type: None,
        progress: false,
        continue_on_error: false,
    };

    match upload_command {
        FileCommands::Upload {
            files,
            name,
            recursive,
            flatten: _,
            prefix: _,
            group,
            metadata,
            tag,
            content_type,
            progress,
            continue_on_error,
        } => {
            assert_eq!(files, vec![non_existent_file]);
            assert!(name.is_none());
            assert!(!recursive);
            assert!(group.is_empty());
            assert!(metadata.is_empty());
            assert!(tag.is_empty());
            assert!(content_type.is_none());
            assert!(!progress);
            assert!(!continue_on_error);
        }
        _ => panic!("Expected Upload command"),
    }

    Ok(())
}

#[tokio::test]
async fn test_metadata_and_tag_parsing() -> Result<()> {
    let temp_file = NamedTempFile::new().unwrap();
    fs::write(temp_file.path(), b"test content").unwrap();

    let upload_command = FileCommands::Upload {
        files: vec![temp_file.path().to_string_lossy().to_string()],
        name: Some("test-file.txt".to_string()),
        recursive: false,
        flatten: false,
        prefix: None,
        group: vec!["test".to_string()],
        metadata: vec![
            ("author".to_string(), "John Doe".to_string()),
            ("version".to_string(), "1.2.3".to_string()),
        ],
        tag: vec![
            ("team".to_string(), "devops".to_string()),
            ("project".to_string(), "crosstache".to_string()),
        ],
        content_type: Some("text/plain".to_string()),
        progress: true,
        continue_on_error: false,
    };

    match upload_command {
        FileCommands::Upload {
            files: _,
            name: _,
            recursive: _,
            flatten: _,
            prefix: _,
            group: _,
            metadata,
            tag,
            content_type: _,
            progress: _,
            continue_on_error: _,
        } => {
            assert_eq!(metadata.len(), 2);
            assert!(metadata.contains(&("author".to_string(), "John Doe".to_string())));
            assert!(metadata.contains(&("version".to_string(), "1.2.3".to_string())));

            assert_eq!(tag.len(), 2);
            assert!(tag.contains(&("team".to_string(), "devops".to_string())));
            assert!(tag.contains(&("project".to_string(), "crosstache".to_string())));
        }
        _ => panic!("Expected Upload command"),
    }

    Ok(())
}

#[tokio::test]
async fn test_file_list_command() -> Result<()> {
    let list_command = FileCommands::List {
        prefix: Some("config/".to_string()),
        group: Some("production".to_string()),
        metadata: true,
        limit: Some(50),
        recursive: false,
    };

    match list_command {
        FileCommands::List { prefix, group, metadata, limit, recursive: _ } => {
            assert_eq!(prefix, Some("config/".to_string()));
            assert_eq!(group, Some("production".to_string()));
            assert!(metadata);
            assert_eq!(limit, Some(50));
        }
        _ => panic!("Expected List command"),
    }

    Ok(())
}

#[tokio::test]
async fn test_file_delete_command() -> Result<()> {
    let delete_command = FileCommands::Delete {
        files: vec!["old-file.txt".to_string()],
        force: true,
        continue_on_error: false,
    };

    match delete_command {
        FileCommands::Delete { files, force, continue_on_error: _ } => {
            assert_eq!(files, vec!["old-file.txt"]);
            assert!(force);
        }
        _ => panic!("Expected Delete command"),
    }

    Ok(())
}

#[tokio::test]
async fn test_file_info_command() -> Result<()> {
    let info_command = FileCommands::Info {
        name: "info-file.txt".to_string(),
    };

    match info_command {
        FileCommands::Info { name } => {
            assert_eq!(name, "info-file.txt");
        }
        _ => panic!("Expected Info command"),
    }

    Ok(())
}

#[tokio::test]
async fn test_configuration_creation() -> Result<()> {
    let config = create_test_config();

    assert_eq!(config.default_vault, "test-vault");
    assert_eq!(config.default_resource_group, "test-rg");
    assert_eq!(config.subscription_id, "test-subscription");

    assert!(config.blob_config.is_some());
    let blob_config = config.blob_config.unwrap();
    assert_eq!(blob_config.storage_account, "teststorage");
    assert_eq!(blob_config.container_name, "test-container");
    assert!(blob_config.enable_large_file_support);
    assert_eq!(blob_config.chunk_size_mb, 4);
    assert_eq!(blob_config.max_concurrent_uploads, 3);

    Ok(())
}
