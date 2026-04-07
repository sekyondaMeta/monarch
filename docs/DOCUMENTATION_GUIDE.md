# Monarch Documentation System Guide

This guide explains how the Monarch project's documentation system works and how to contribute to it.

## Quick Start - Building Documentation

### Building Locally

For most users, this is all you need to get started:

```bash
# From project root
cd docs

# Install dependencies
pip install -r requirements.txt

# Build all documentation
make html

# View the results
open build/html/index.html
```

The generated documentation will be available in `docs/build/html/`.

### CI/CD Builds

The documentation is automatically built and deployed via GitHub Actions (`.github/workflows/doc_build.yml`):

| Trigger | Version | Deploys? |
|---------|---------|----------|
| Push to `docs/*` branch (e.g. `docs/v0.5.0`) | Extracted from branch name |  Yes |
| Manual `workflow_dispatch` with `version` input | User-provided |  Yes |
| Pull request | `dev` |  Build only |

Pre-release versions (e.g. `v0.4.0rc1`, `v1.0.0beta2`) are detected and skipped during deploy.

### Development Builds

For faster iteration during development:

```bash
# Clean previous builds
make clean

# Build with CI environment (if you have full Monarch built)
CI=true make html

# Standard build (uses mocked imports for unavailable Rust bindings)
make html
```

## Overview

The Monarch documentation system is built using **Sphinx** (for Python documentation) with additional support for **Rust documentation** (via `cargo doc`) and **mdBook** (for narrative books). The system automatically generates API documentation, includes examples, and supports multiple output formats.

## Documentation Structure

```
docs/
├── Makefile              # Main build commands
├── requirements.txt      # Python dependencies for docs
├── source/
│   ├── conf.py          # Sphinx configuration
│   ├── index.md         # Main documentation homepage
│   ├── get_started.md   # Getting started guide
│   ├── rust-api.md      # Rust API overview page
│   ├── _ext/            # Custom Sphinx extensions
│   │   └── public_api_generator.py  # Generates Python API docs
│   ├── api/             # Generated Python API documentation
│   ├── books/           # mdBook-based narrative documentation
│   │   └── hyperactor-book/  # The Hyperactor book
│   └── examples/        # Python example files
└── build/               # Generated documentation output
```

## How the Documentation System Works

### 1. Python API Documentation

The Python API documentation is automatically generated from the codebase using a multi-step process:

1. **Public API Detection**: The system reads the `__all__` list and `_public_api` dictionary from the main `monarch` package
2. **Categorization**: APIs are automatically categorized into logical groups (Core Components, Mesh Management, Data Operations, etc.)
3. **Sphinx Generation**: Uses Sphinx's `autodoc` extension to extract docstrings and generate formatted documentation
4. **Custom Extension**: The `public_api_generator.py` extension handles the organization and template generation

**Key Files:**
- `docs/source/_ext/public_api_generator.py` - Custom extension for API doc generation
- `docs/source/conf.py` - Lines 50-90 handle autodoc configuration
- Generated output: `docs/source/api/index.rst`

### 2. Rust API Documentation

Rust documentation is generated using `cargo doc` and integrated into the Sphinx build:

1. **Cargo Doc Generation**: `cargo doc --workspace --no-deps` generates HTML documentation for all Rust crates
2. **Integration**: The generated docs are copied to `docs/source/target/doc/` and served alongside Sphinx docs
3. **Navigation Page**: `docs/source/rust-api.md` provides organized access to all crate docs

**Build Process:**
```bash
# Generate Rust documentation
cargo doc --workspace --no-deps

# Copy to docs directory
mkdir -p docs/source/target docs/build/html/rust-api
cp -r target/doc docs/source/target/
```

### 3. Books (mdBook)

Narrative documentation is created using **mdBook** for rich, book-like content:

- **Location**: `docs/source/books/hyperactor-book/`
- **Configuration**: `book.toml` defines book metadata and build settings
- **Content**: Markdown files in the `src/` directory organized by `SUMMARY.md`
- **Integration**: Books are built separately and integrated into the main documentation site

### 4. Examples

Python examples are automatically processed by **Sphinx Gallery**:
- **Source**: `docs/source/examples/`
- **Processing**: Sphinx Gallery converts `.py` files into documented examples
- **Output**: Generated gallery in `docs/build/html/generated/examples/`

## How to Add to the Documentation

### Adding to Python API Documentation

1. **Ensure proper docstrings**: Add comprehensive docstrings to your Python functions/classes:
   ```python
   def my_function(param: str) -> int:
       """
       Brief description of the function.

       Args:
           param: Description of the parameter

       Returns:
           Description of return value

       Example:
           >>> my_function("test")
           42
       """
       return 42
   ```

2. **Update `__all__` and `_public_api`**: If adding new public APIs, update the package's `__all__` list and `_public_api` dictionary in the main `__init__.py` file.

3. **Rebuild documentation**: The API docs will be automatically regenerated on the next build.

### Adding to Rust API Documentation

1. **Add comprehensive doc comments** to your Rust code:
   ```rust
   /// Brief description of the function.
   ///
   /// # Arguments
   ///
   /// * `param` - Description of the parameter
   ///
   /// # Returns
   ///
   /// Description of return value
   ///
   /// # Examples
   ///
   /// ```
   /// let result = my_function("test");
   /// assert_eq!(result, 42);
   /// ```
   pub fn my_function(param: &str) -> i32 {
       42
   }
   ```

2. **Update crate documentation**: Add or update the crate-level documentation in `lib.rs`:
   ```rust
   //! # My Crate
   //!
   //! Description of what this crate does.
   ```

3. **Rebuild Rust docs**: Run `cargo doc --workspace --no-deps` to regenerate documentation.

### Adding to Books

1. **Navigate to the book directory**: `docs/source/books/hyperactor-book/`

2. **Create or edit markdown files**: Add content to existing files or create new ones in the `src/` directory.

3. **Update the table of contents**: Edit `src/SUMMARY.md` to include new pages:
   ```markdown
   # Summary

   - [Introduction](introduction.md)
   - [New Chapter](new_chapter.md)
   ```

4. **Build the book**: Run `mdbook build` in the book directory to generate HTML.

### Adding Examples

1. **Create Python example files** in `docs/source/examples/`:
   ```python
   """
   Title of the Example
   ====================

   Description of what this example demonstrates.
   """

   import monarch

   # Your example code here
   print("Hello, Monarch!")
   ```

2. **Follow naming conventions**: Use descriptive filenames that will make sense in the generated gallery.

3. **Include comprehensive docstrings**: The first docstring becomes the example description.

## Build Process Details

The documentation build process includes several steps:

1. **Environment Setup**: The build script `scripts/build_monarch_for_docs.sh` ensures Monarch is properly built with Rust bindings
2. **API Generation**: Custom extensions generate Python API documentation
3. **Rust Integration**: Cargo doc output is copied and integrated
4. **Sphinx Build**: Main documentation is generated using Sphinx
5. **Asset Copying**: Static files and generated docs are organized for serving

### CI/CD Integration

The documentation is automatically built and deployed via GitHub Actions (see `.github/workflows/doc_build.yml`). See [Versioning & Deployment](#versioning--deployment) below for full details.

## Versioning & Deployment

### How Versioned Docs Work

Documentation is deployed to **GitHub Pages** via the `gh-pages` branch at `https://meta-pytorch.org/monarch/`. Each release gets its own directory, and a `stable` symlink always points to the latest stable release.

```
gh-pages branch:
├── index.html          ← redirects to stable/
├── versions.json       ← powers the version switcher dropdown
├── stable              ← symlink → v0.5.0 (latest stable)
├── .nojekyll
├── v0.5.0/             ← latest stable release docs
├── v0.4.0/             ← previous release docs
└── v0.3.0/
```

**Key URLs:**
- `https://meta-pytorch.org/monarch/` — redirects to `/stable/`
- `https://meta-pytorch.org/monarch/stable/` — always serves the latest stable release
- `https://meta-pytorch.org/monarch/v0.5.0/` — specific version (permanent)

### Release Docs Pipeline

When a stable release is published, the full pipeline is:

```
1. publish_release.yml runs for vX.Y.Z
   └─ Creates branch: docs/vX.Y.Z and pushes to origin

2. doc_build.yml triggers on docs/vX.Y.Z branch push
   ├─ Build job (GPU runner):
   │   ├─ Builds Monarch from source with Rust bindings
   │   ├─ Runs cargo doc --workspace --no-deps
   │   ├─ Runs make html (Sphinx)
   │   └─ Uploads docs artifact
   └─ Deploy job:
       ├─ Checks out gh-pages branch
       ├─ Copies built HTML into gh-pages/vX.Y.Z/
       ├─ Updates versions.json (marks vX.Y.Z as preferred/stable)
       ├─ Creates symlink: gh-pages/stable → vX.Y.Z
       ├─ Writes gh-pages/index.html redirecting to stable/
       └─ Commits and pushes to gh-pages
```

### How the Stable Symlink Works

The `stable` symlink is a filesystem symlink committed into Git on the `gh-pages` branch. GitHub Pages respects Git symlinks, so `https://meta-pytorch.org/monarch/stable/` transparently serves the content from the version directory it points to.

The symlink is **updated automatically** on every versioned deploy. The deploy job:

1. Rebuilds `versions.json` with all known versions, sorted by semver descending
2. Identifies the highest non-pre-release version as the latest stable
3. Removes the old symlink and creates a new one pointing to the latest stable
4. Updates the root `index.html` to redirect to `stable/`

This means when a new release (e.g. `v1.0.0`) is deployed, the symlink automatically swings from the previous stable (e.g. `v0.5.0`) to the new one. No manual intervention is required.

This approach mirrors the pattern used by PyTorch core (`pytorch.org/docs/stable/`) and other ecosystem libraries (TorchVision, TorchAudio, ExecuTorch).

### Version Switcher

The version dropdown in the docs navbar is powered by `versions.json`, which is dynamically generated at deploy time. The configuration in `conf.py`:

```python
"switcher": {
    "json_url": "https://meta-pytorch.org/monarch/versions.json",
    "version_match": docs_version,
},
```

Each entry in `versions.json` has:
- `name` — display label (e.g. `"v0.5.0 (stable)"`)
- `version` — version string for matching
- `url` — link to that version's docs
- `preferred` — `true` only for the latest stable release

### Version Detection

The `DOCS_VERSION` environment variable controls what version is built. It is set automatically based on the trigger:

| Trigger | `DOCS_VERSION` value |
|---------|---------------------|
| `docs/v0.5.0` branch push | `v0.5.0` (from branch name) |
| Manual dispatch with `version: v0.5.0` | `v0.5.0` (from input) |
| Scheduled build (currently disabled) | `nightly` |
| Pull request / other | `dev` |

In `conf.py`, this value is read via `os.getenv("DOCS_VERSION", "dev")` and used for Sphinx's `version` and `release` fields, and for matching the version switcher.

### Pre-release Handling

Pre-release versions (matching the pattern `v0.4.0rc1`, `v1.0.0beta2`, etc.) are automatically **skipped during deploy** — the build runs but the deploy step exits early. This prevents release candidates from appearing in the public docs.

### Manual Deployment

To manually trigger a docs deploy for a specific version:

```bash
# Via GitHub Actions workflow dispatch
gh workflow run doc_build.yml -f version=v0.5.0
```

### Updating Docs for an Existing Release

To fix docs for an already-published release:

1. Cherry-pick or commit fixes to the `docs/vX.Y.Z` branch
2. Push the branch — `doc_build.yml` triggers automatically
3. The deploy job overwrites `gh-pages/vX.Y.Z/` with the updated build

## Configuration Details

### Sphinx Configuration (`conf.py`)

Key configuration sections:

- **Extensions**: Includes `sphinx.ext.autodoc`, `myst_parser`, `sphinx_gallery.gen_gallery`, and custom extensions
- **Theme**: Uses `pytorch_sphinx_theme2` for consistent PyTorch ecosystem styling
- **API Generation**: Custom `public_api_generator` extension with automatic categorization
- **Mock Imports**: Environment-dependent mocking for Rust bindings during local development

### Build Dependencies

**Python packages** (from `docs/requirements.txt`):
- Sphinx and extensions
- Theme packages
- MyST parser for Markdown
- Sphinx Gallery for examples

**System dependencies**:
- Rust toolchain (for `cargo doc`)
- mdBook (for book generation)
- Complete Monarch build environment

## Troubleshooting

### Common Issues

1. **Import Errors**: Ensure Monarch is properly installed with `python setup.py develop`
2. **Missing Rust Docs**: Run `cargo doc --workspace --no-deps` before building
3. **Theme Issues**: Check that all theme dependencies are installed
4. **Build Failures**: Use `make clean` then `make html` for a fresh build

### Environment Differences

- **CI Environment**: Uses real Monarch imports, no mocking required
- **Local Development**: May use mocked imports for unavailable Rust bindings
- **Debug Mode**: Set `CI=true` environment variable to disable mocking locally

### Getting Help

- Check the GitHub Actions logs for CI build issues
- Examine `scripts/handle_sphinx_errors.sh` for error handling
- Review Sphinx documentation for advanced configuration options

This documentation system provides comprehensive, automatically-generated documentation that stays in sync with the codebase while supporting rich narrative content through books and examples.
