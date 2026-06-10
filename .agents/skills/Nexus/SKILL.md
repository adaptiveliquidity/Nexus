```markdown
# Nexus Development Patterns

> Auto-generated skill from repository analysis

## Overview
This skill teaches the core development patterns and conventions used in the Nexus Rust codebase. You'll learn how to structure files, write imports and exports, follow commit message conventions, and understand the project's approach to testing. This guide is ideal for new contributors or anyone aiming to maintain consistency in the Nexus repository.

## Coding Conventions

### File Naming
- Use **snake_case** for all file and module names.
  - Example:  
    ```rust
    mod user_profile;
    pub mod data_manager;
    ```

### Import Style
- Use **relative imports** within the codebase.
  - Example:  
    ```rust
    use crate::utils::parser;
    use super::config;
    ```

### Export Style
- Use **named exports** for modules and functions.
  - Example:  
    ```rust
    pub fn process_data() { ... }
    pub mod handlers;
    ```

### Commit Messages
- Follow **Conventional Commits** with the following prefixes:
  - `feat`: For new features
  - `fix`: For bug fixes
- Keep commit messages concise (average ~62 characters).
  - Example:  
    ```
    feat: add user authentication middleware
    fix: correct data parsing in import handler
    ```

## Workflows

### Feature Development
**Trigger:** When adding a new feature  
**Command:** `/feature`

1. Create a new branch for your feature.
2. Implement the feature using snake_case file naming and relative imports.
3. Write or update tests as needed.
4. Commit your changes with a `feat:` prefix and a concise message.
5. Open a pull request for review.

### Bug Fixing
**Trigger:** When fixing a bug  
**Command:** `/fix`

1. Create a new branch for your bugfix.
2. Make the necessary code changes, following file and import conventions.
3. Update or add tests to cover the fix.
4. Commit your changes with a `fix:` prefix and a concise message.
5. Open a pull request for review.

## Testing Patterns

- Test files follow the `*.test.*` pattern (e.g., `user_profile.test.rs`).
- The specific testing framework is not detected; follow Rust's standard testing conventions.
- Example test file:
  ```rust
  #[cfg(test)]
  mod tests {
      use super::*;

      #[test]
      fn test_process_data() {
          assert_eq!(process_data(), expected_value);
      }
  }
  ```

## Commands
| Command    | Purpose                       |
|------------|------------------------------|
| /feature   | Start a new feature workflow  |
| /fix       | Start a bugfix workflow       |
```