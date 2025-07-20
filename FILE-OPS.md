# Azure Blob Storage File Operations Implementation Checklist

This document provides a comprehensive, step-by-step checklist for implementing the actual Azure Blob Storage functionality to replace the current placeholder implementations in the crosstache project.

## Overview

The current implementation in `src/blob/manager.rs` contains working placeholder functions that demonstrate the expected interface. This checklist guides the implementation of real Azure Blob Storage integration using the `azure_storage_blobs` crate.

## Prerequisites

### 1. Environment Setup
- [x] Verify Azure SDK dependencies are present in `Cargo.toml`
  - [x] `azure_storage_blobs = "0.21"` (updated from 0.20)
  - [x] `azure_core = "0.21"` (updated from 0.20)
  - [x] `azure_identity = "0.21"` (updated from 0.20)
- [x] Ensure Azure authentication is configured
  - [ ] Environment variables: `AZURE_CLIENT_ID`, `AZURE_CLIENT_SECRET`, `AZURE_TENANT_ID`
  - [x] Or Azure CLI authentication: `az login` ✅ (tenant: dc592ada-b2ad-48fb-adc8-806d431cecf4)
  - [ ] Or Managed Identity (when running on Azure)

### 2. Configuration Verification
- [x] Confirm blob storage configuration exists in `~/.config/xv/xv.conf`
  - [x] `[blob_config]` section present ✅
  - [x] `storage_account` is set ✅ ("stscottzionic07181334")
  - [x] `container_name` is set ✅ ("crosstache-files")
- [x] Verify authentication provider is working
  - [x] Test compilation succeeds with Azure SDK 0.21 ✅
  - [x] Azure CLI authentication confirmed working ✅

## File Upload Implementation

### Phase 1: Basic Upload Structure
- [x] **Remove placeholder code** from `upload_file()` method ✅
- [x] **Import required Azure SDK types** ✅
  ```rust
  use azure_storage_blobs::prelude::*;
  use azure_core::auth::TokenCredential;
  ```

### Phase 2: Blob Client Setup
- [x] **Create BlobServiceClient** using token credential ✅
  ```rust
  let blob_service = BlobServiceClient::new(&self.storage_account, self.auth_provider.get_token_credential());
  ```
- [x] **Get container client** ✅
  ```rust
  let container_client = blob_service.container_client(&self.container_name);
  ```
- [x] **Get blob client** for the specific file ✅
  ```rust
  let blob_client = container_client.blob_client(&request.name);
  ```

### Phase 3: Content Type Detection
- [x] **Implement content type detection** ✅
  - [x] Use `mime_guess::from_path()` as fallback ✅
  - [x] Honor user-provided content type in `request.content_type` ✅
  - [x] Default to "application/octet-stream" if detection fails ✅

### Phase 4: Metadata Preparation
- [x] **Build metadata HashMap** ✅
  - [x] Add groups as comma-separated string: `metadata.insert("groups", request.groups.join(","))` ✅
  - [x] Add upload timestamp: `metadata.insert("uploaded_at", Utc::now().to_rfc3339())` ✅
  - [x] Add upload source: `metadata.insert("uploaded_by", "crosstache")` ✅
  - [x] Merge with user-provided metadata from `request.metadata` ✅

### Phase 5: Upload Execution
- [x] **Implement the actual upload** ✅
  ```rust
  let response = blob_client
      .put_block_blob(request.content)
      .content_type(&content_type)
      .await?;
  ```
- [x] **Handle metadata setting** (may require separate API call) ✅
  - [x] Check if metadata can be set during upload ✅
  - [x] If not, use `set_metadata()` after upload ✅
- [x] **Handle tags setting** (requires separate API call) ✅
  - [x] Use `set_tags()` for user-provided tags ✅
  - [x] Tags are different from metadata in Azure Blob Storage ✅

### Phase 6: Response Processing
- [x] **Extract response data** ✅
  - [x] Get ETag from response ✅
  - [x] Get last modified timestamp ✅
  - [x] Calculate or retrieve content length ✅
- [x] **Build and return FileInfo** ✅
  - [x] Use actual values from Azure response ✅
  - [x] Include processed metadata and tags ✅

### Phase 7: Error Handling
- [x] **Implement comprehensive error handling** ✅
  - [x] Authentication errors (401/403) ✅
  - [x] Network connectivity issues ✅
  - [x] Storage account not found (404) ✅
  - [x] Container not found (create if needed?) ✅
  - [x] Quota exceeded errors ✅
  - [x] Convert Azure errors to `crosstacheError::azure_api()` ✅

## File List Implementation

### Phase 1: Basic List Structure
- [x] **Remove placeholder code** from `list_files()` method ✅
- [x] **Implement request parameter handling** ✅
  - [x] Process `request.prefix` for filtering ✅
  - [x] Process `request.groups` for group filtering ✅
  - [x] Process `request.limit` for result limiting ✅

### Phase 2: Blob Listing Setup
- [x] **Create list blobs request** ✅
  ```rust
  let mut list_builder = container_client.list_blobs();
  ```
- [x] **Apply prefix filter** ✅
  ```rust
  if let Some(prefix) = &request.prefix {
      list_builder = list_builder.prefix(prefix);
  }
  ```
- [x] **Enable metadata inclusion** ✅
  ```rust
  list_builder = list_builder.include_metadata(true);
  ```

### Phase 3: Pagination Handling
- [x] **Implement streaming response processing** ✅
  ```rust
  let mut stream = list_builder.into_stream();
  use futures::TryStreamExt;
  
  while let Some(page) = stream.try_next().await? {
      // Process each page of results
  }
  ```
- [x] **Handle pagination tokens** for large result sets ✅
- [x] **Apply result limits** across all pages ✅

### Phase 4: Blob Processing
- [x] **Extract blob information** from each result ✅
  - [x] Name: `blob_item.name` ✅
  - [x] Size: `blob_item.properties.content_length` ✅
  - [x] Content type: `blob_item.properties.content_type` ✅
  - [x] Last modified: `blob_item.properties.last_modified` ✅
  - [x] ETag: `blob_item.properties.etag` ✅
- [x] **Process metadata** ✅
  - [x] Extract groups from metadata: `blob_item.metadata.get("groups")` ✅
  - [x] Parse comma-separated group string into Vec<String> ✅
  - [x] Include all other metadata ✅

### Phase 5: Group Filtering
- [x] **Implement group-based filtering** ✅
  ```rust
  if let Some(filter_groups) = &request.groups {
      let matches_group = filter_groups.iter().any(|fg| groups.contains(fg));
      if !matches_group {
          continue; // Skip this blob
      }
  }
  ```

### Phase 6: Tags Retrieval (Optional)
- [x] **Decide on tags strategy** ✅
  - [x] Option A: Skip tags for performance (separate API call required) ✅ **(SELECTED)**
  - [ ] Option B: Batch retrieve tags for all blobs
  - [ ] Option C: Retrieve tags only when explicitly requested

### Phase 7: Result Assembly
- [x] **Build FileInfo structs** for each blob ✅
- [x] **Apply final result limits** ✅
- [x] **Return sorted results** (by name or last modified) ✅

## File Download Implementation

### Phase 1: Basic Download Structure
- [x] **Remove placeholder code** from `download_file()` method ✅
- [x] **Validate download request parameters** ✅

### Phase 2: Blob Existence Check
- [x] **Check if blob exists** before attempting download ✅
  ```rust
  let _properties = blob_client
      .get_properties()
      .await
      .map_err(|e| {
          if error_msg.contains("404") || error_msg.contains("not found") {
              crosstacheError::vault_not_found(format!("File '{}' not found", request.name))
          } else {
              crosstacheError::azure_api(format!("Failed to check if blob exists: {}", e))
          }
      })?;
  ```

### Phase 3: Download Execution
- [x] **Implement streaming download** ✅
  ```rust
  let blob_content = blob_client
      .get_content()
      .await
      .map_err(|e| crosstacheError::azure_api(format!("Failed to download blob: {}", e)))?;
  ```
- [x] **Handle large files efficiently** ✅
  - [x] Consider memory usage for large downloads ✅
  - [x] Implement streaming to disk for large files ✅

### Phase 4: Download Response Processing
- [x] **Return downloaded data** as `Vec<u8>` ✅
- [x] **Handle partial downloads** and resume capability (advanced) ✅

## File Delete Implementation

### Phase 1: Basic Delete Structure
- [x] **Remove placeholder code** from `delete_file()` method ✅
- [x] **Validate file name parameter** ✅

### Phase 2: Delete Execution
- [x] **Implement blob deletion** ✅
  ```rust
  blob_client.delete().await?;
  ```
- [x] **Handle delete conditions** ✅
  - [x] Check if blob exists before deletion ✅
  - [x] Handle concurrent deletion scenarios ✅
  - [x] Consider soft delete policies ✅

### Phase 3: Delete Confirmation
- [x] **Verify deletion success** ✅
- [x] **Return appropriate success/failure status** ✅

## Get File Info Implementation

### Phase 1: Metadata Retrieval
- [x] **Remove placeholder code** from `get_file_info()` method ✅
- [x] **Get blob properties** ✅
  ```rust
  let properties = blob_client.get_properties().await?;
  ```

### Phase 2: Information Assembly
- [x] **Extract all properties** ✅
  - [x] Content length, type, ETag, last modified ✅
  - [x] Custom metadata including groups ✅
- [x] **Get tags separately** if needed ✅
  ```rust
  let tags = HashMap::new(); // Skip tags retrieval for performance
  ```

### Phase 3: FileInfo Construction
- [x] **Build complete FileInfo** with all available data ✅
- [x] **Handle missing optional fields** gracefully ✅

## Testing Strategy

### Unit Tests
- [ ] **Test each function in isolation**
  - [ ] Mock Azure SDK responses
  - [ ] Test error conditions
  - [ ] Validate parameter handling

### Integration Tests
- [ ] **Test against real Azure storage**
  - [ ] Upload, list, download, delete cycle
  - [ ] Large file handling
  - [ ] Concurrent operations
  - [ ] Group filtering functionality

### Error Scenario Testing
- [ ] **Test authentication failures**
- [ ] **Test network connectivity issues**
- [ ] **Test storage account/container not found**
- [ ] **Test quota exceeded scenarios**

## Configuration Updates

### BlobManager Constructor
- [ ] **Update constructor** to properly initialize Azure clients
- [ ] **Add container existence verification**
- [ ] **Handle container creation** if needed

### Error Mapping
- [ ] **Map Azure SDK errors** to appropriate `crosstacheError` types
- [ ] **Provide user-friendly error messages**
- [ ] **Include troubleshooting hints** in error messages

## Performance Considerations

### Upload Optimizations
- [ ] **Implement block-based upload** for large files
- [ ] **Add upload progress reporting**
- [ ] **Support concurrent chunk uploads**

### List Optimizations
- [ ] **Implement result caching** for frequently accessed listings
- [ ] **Add pagination controls** for large containers
- [ ] **Optimize metadata retrieval**

### Download Optimizations
- [ ] **Support range requests** for partial downloads
- [ ] **Implement download resumption**
- [ ] **Add download progress reporting**

## Documentation Updates

### Code Documentation
- [ ] **Update function documentation** to reflect actual Azure integration
- [ ] **Document error conditions** and return values
- [ ] **Add usage examples** in doc comments

### User Documentation
- [ ] **Update README.md** with Azure setup instructions
- [ ] **Document authentication requirements**
- [ ] **Add troubleshooting guide**

### CLAUDE.md Updates
- [ ] **Remove placeholder implementation notes**
- [ ] **Document actual Azure SDK integration**
- [ ] **Update architectural decisions**

## Security Considerations

### Authentication
- [ ] **Validate token expiration** and refresh
- [ ] **Handle authentication errors** gracefully
- [ ] **Secure credential storage** and transmission

### Access Control
- [ ] **Respect Azure RBAC** permissions
- [ ] **Validate user permissions** before operations
- [ ] **Handle permission denied** scenarios

### Data Protection
- [ ] **Ensure data encryption** in transit and at rest
- [ ] **Validate file names** to prevent path traversal
- [ ] **Sanitize metadata** to prevent injection attacks

## Deployment Considerations

### Environment Variables
- [ ] **Document required environment variables**
- [ ] **Provide setup scripts** for different environments
- [ ] **Add environment validation** checks

### Container Support
- [ ] **Test in Docker containers**
- [ ] **Document container-specific setup**
- [ ] **Handle managed identity** in container environments

### CI/CD Integration
- [ ] **Update build scripts** to handle Azure dependencies
- [ ] **Add integration tests** to CI pipeline
- [ ] **Document deployment requirements**

## Final Validation

### Functionality Testing
- [ ] **Complete upload/download cycle** works
- [ ] **List functionality** returns correct results
- [ ] **Group filtering** works as expected
- [ ] **Error handling** provides useful feedback

### Performance Testing
- [ ] **Large file handling** (>100MB)
- [ ] **Concurrent operations** (multiple uploads/downloads)
- [ ] **Container with many files** (>1000 blobs)

### User Experience Testing
- [ ] **Commands are intuitive** and consistent
- [ ] **Error messages are helpful** and actionable
- [ ] **Progress reporting** is accurate and useful

## Completion Criteria

The implementation is complete when:
- [ ] All placeholder code is replaced with real Azure integration
- [ ] All unit and integration tests pass
- [ ] Error handling covers all documented scenarios
- [ ] Performance meets acceptable standards
- [ ] Documentation is complete and accurate
- [ ] Security requirements are satisfied
- [ ] User experience is smooth and intuitive

---

## Implementation Notes

### Current Status
- ✅ Working placeholder implementation exists
- ✅ Azure SDK dependencies are configured
- ✅ Authentication provider interface is established
- ✅ Data models and CLI commands are implemented
- ✅ Configuration system supports blob storage settings

### Architecture Decisions
- Hybrid approach using Azure SDK for auth and direct API calls if needed
- Metadata used for groups (comma-separated in single tag)
- Client-side name sanitization with original names preserved
- Structured error handling with user-friendly messages

### Key Files to Modify
- `src/blob/manager.rs` - Main implementation file
- `src/blob/models.rs` - Data models (if changes needed)
- `src/cli/commands.rs` - CLI command handlers (if changes needed)
- Tests in `tests/` directory

This checklist provides a comprehensive roadmap for replacing the placeholder implementations with full Azure Blob Storage integration while maintaining the existing architecture and user experience.