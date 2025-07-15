# Interactive Setup Implementation Checklist

This checklist provides step-by-step tasks for implementing the `xv init` command with interactive setup and first-time user experience improvements.

## Phase 1: Analysis & Architecture

### □ Step 1: Analyze Current Configuration System
- [ ] Read and document `/src/config/settings.rs` configuration loading/saving mechanisms
- [ ] Read and document `/src/config/context.rs` vault context management
- [ ] Analyze environment variable handling in config system
- [ ] Review existing CLI command patterns in `/src/cli/commands.rs`
- [ ] Understand current argument parsing and validation approaches
- [ ] Document current error handling patterns
- [ ] Create list of required vs optional configuration fields
- [ ] Identify current setup workflow pain points

### □ Step 2: Design Interactive Setup Architecture
- [ ] Define command structure: `xv init [--force] [--profile NAME] [--non-interactive] [--minimal]`
- [ ] Design setup flow stages (welcome → detection → subscription → resource group → vault → test vault → summary)
- [ ] Plan user interaction patterns (progress indicators, confirmations, validation, error recovery)
- [ ] Create command specification document
- [ ] Design error handling strategy for each stage

## Phase 2: Core Implementation

### □ Step 3: Implement Azure Environment Detection
- [ ] Create `/src/cli/setup/detection.rs` module
- [ ] Define `AzureEnvironment` struct with fields: `has_azure_cli`, `subscription_id`, `subscription_name`, `tenant_id`, `default_location`, `available_subscriptions`
- [ ] Implement `detect_azure_environment() -> Result<AzureEnvironment>` function
- [ ] Implement `verify_azure_cli_login() -> Result<bool>` function
- [ ] Implement `get_current_subscription() -> Result<Option<Subscription>>` function
- [ ] Implement `list_available_subscriptions() -> Result<Vec<Subscription>>` function
- [ ] Add Azure CLI installation check (`az --version`)
- [ ] Add Azure CLI login status check (`az account show`)
- [ ] Add subscription and tenant detection
- [ ] Handle detection errors: CLI not installed, not logged in, network issues, permission problems
- [ ] Write unit tests for detection logic

### □ Step 4: Create Interactive Prompting System
- [ ] Create `/src/cli/setup/prompts.rs` module
- [ ] Define `Prompt` trait with `type Output` and `async fn prompt(&self) -> Result<Self::Output>`
- [ ] Implement `YesNoPrompt` struct with `message` and `default` fields
- [ ] Implement `SelectPrompt<T>` struct with `message`, `options`, and `default` fields
- [ ] Implement `TextPrompt` struct with `message`, `default`, and `validator` fields
- [ ] Add yes/no confirmation prompts
- [ ] Add single selection from list prompts
- [ ] Add text input with validation prompts
- [ ] Add secure password input prompts
- [ ] Implement consistent prompt styling and formatting
- [ ] Add progress indicators and success confirmations
- [ ] Add error message display functionality

## Phase 3: Configuration & Setup Logic

### □ Step 5: Implement Configuration Generation
- [ ] Create `/src/cli/setup/config_builder.rs` module
- [ ] Define `SetupConfigBuilder` struct with `config` and `detected_env` fields
- [ ] Implement `new(detected_env: AzureEnvironment) -> Self`
- [ ] Implement `with_subscription(mut self, subscription_id: String) -> Self`
- [ ] Implement `with_resource_group(mut self, rg: String) -> Self`
- [ ] Implement `with_default_vault(mut self, vault: String) -> Self`
- [ ] Implement `build(self) -> Result<Config>`
- [ ] Implement `save_config(&self, config: &Config) -> Result<()>`
- [ ] Add smart defaults based on detected environment
- [ ] Implement configuration validation (subscription access, resource group existence, vault name availability)
- [ ] Add authentication setup testing

### □ Step 6: Add Test Vault Creation Workflow
- [ ] Create `/src/cli/setup/vault_creation.rs` module
- [ ] Define `TestVaultCreator` struct with `vault_manager` and `config` fields
- [ ] Implement `suggest_vault_name(&self) -> String`
- [ ] Implement `create_test_vault(&self, name: &str) -> Result<VaultSummary>`
- [ ] Implement `add_test_secrets(&self, vault_name: &str) -> Result<()>`
- [ ] Implement `cleanup_on_error(&self, vault_name: &str) -> Result<()>`
- [ ] Add unique vault name suggestion logic
- [ ] Implement vault creation with appropriate permissions
- [ ] Add sample secrets for testing
- [ ] Add vault accessibility verification
- [ ] Implement rollback capabilities and error cleanup
- [ ] Add manual cleanup instructions for failures

## Phase 4: Integration & Polish

### □ Step 7: Implement Main Init Command
- [ ] Create `/src/cli/commands/init.rs` module
- [ ] Define `InitArgs` struct for command arguments
- [ ] Implement `execute_init_command(args: InitArgs) -> Result<()>`
- [ ] Add welcome banner and overview display
- [ ] Integrate environment detection with user feedback
- [ ] Implement interactive setup flow orchestration
- [ ] Add configuration saving with confirmation
- [ ] Integrate optional test vault creation
- [ ] Add final setup summary display
- [ ] Update CLI command enum to include `Init`
- [ ] Add argument parsing for init command
- [ ] Register init command with main dispatcher
- [ ] Implement step-by-step progression with error handling

### □ Step 8: Create Comprehensive Testing
- [ ] Write unit tests for environment detection edge cases
- [ ] Write unit tests for prompt interaction simulation
- [ ] Write unit tests for configuration validation scenarios
- [ ] Write unit tests for vault creation error handling
- [ ] Write integration tests for full init command flow
- [ ] Write integration tests for configuration file generation
- [ ] Write integration tests for Azure API interaction
- [ ] Write integration tests for error recovery scenarios
- [ ] Create manual testing checklist for first-time user experience
- [ ] Create manual testing checklist for existing configuration handling
- [ ] Create manual testing checklist for network connectivity issues
- [ ] Create manual testing checklist for authentication failures

## Phase 5: Documentation & Polish

### □ Step 9: Add Documentation and Help
- [ ] Add comprehensive help text for `xv init` command
- [ ] Create command usage examples
- [ ] Write setup troubleshooting guide
- [ ] Document configuration file format and options
- [ ] Update main README.md with init workflow section
- [ ] Add inline code documentation for all new modules
- [ ] Create user-facing documentation for common setup scenarios

## Implementation Notes

### Dependencies to Add
- [ ] Add interactive prompting library (e.g., `dialoguer` crate)
- [ ] Add progress indicator library if needed
- [ ] Add any additional Azure CLI interaction utilities

### Error Handling Requirements
- [ ] Ensure all functions return `Result<T, crosstacheError>`
- [ ] Use user-friendly error messages with actionable suggestions
- [ ] Implement proper error recovery at each stage
- [ ] Log errors appropriately for debugging

### Testing Requirements
- [ ] Achieve 80%+ code coverage for new modules
- [ ] Test across different Azure CLI versions
- [ ] Test various Azure permission scenarios
- [ ] Test network connectivity issues
- [ ] Test existing configuration conflicts
- [ ] Test on Windows, macOS, and Linux if possible

### Integration Points
- [ ] Update `/src/cli/mod.rs` to include new setup modules
- [ ] Update `/src/main.rs` if needed for new command registration
- [ ] Ensure compatibility with existing configuration system
- [ ] Maintain backward compatibility with existing configs

## Success Criteria
- [ ] New users can complete setup in under 2 minutes
- [ ] 90%+ of Azure CLI setups are detected correctly
- [ ] Clear error messages with actionable solutions for all failure cases
- [ ] Generated configuration works immediately for basic operations
- [ ] All tests pass and coverage meets requirements
- [ ] Complete documentation and help text available

## Final Validation
- [ ] Run `cargo test` and ensure all tests pass
- [ ] Run `cargo clippy` and address any warnings
- [ ] Run `cargo fmt` to ensure consistent formatting
- [ ] Test the complete flow manually as a new user
- [ ] Verify all error scenarios are handled gracefully
- [ ] Confirm documentation is complete and accurate