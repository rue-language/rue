#!/bin/bash
set -euo pipefail

# Developer setup script for Rue
# One-command setup for new developers

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
RUST_PROJECT_SCRIPT="$SCRIPT_DIR/update-rust-project.sh"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Helper function to print colored output
print_status() {
    echo -e "${BLUE}🔧 $1${NC}"
}

print_success() {
    echo -e "${GREEN}✅ $1${NC}"
}

print_error() {
    echo -e "${RED}❌ $1${NC}" >&2
}

print_warning() {
    echo -e "${YELLOW}⚠️  $1${NC}"
}

print_info() {
    echo -e "${BLUE}ℹ️  $1${NC}"
}

show_help() {
    cat << EOF
Rue Developer Setup Script

USAGE:
    $0 [OPTIONS]

OPTIONS:
    -h, --help              Show this help message
    -y, --yes               Assume yes for all prompts (non-interactive mode)
    --skip-dotslash         Skip dotslash installation check/setup
    --skip-rust-project     Skip rust-project.json generation
    --check-only            Only check prerequisites, don't install anything

DESCRIPTION:
    This script sets up everything needed for Rue development:
    
    1. Checks and optionally installs dotslash (required for Buck2)
    2. Generates rust-project.json for Rust-Analyzer IDE support
    3. Verifies Buck2 functionality with a simple build test
    4. Provides next steps for development

    The script is safe to run multiple times and will skip steps that
    are already completed.

EXAMPLES:
    $0                      # Interactive setup with prompts
    $0 -y                   # Non-interactive setup (assume yes)
    $0 --check-only         # Only check what's needed, don't install
    $0 --skip-dotslash      # Skip dotslash, only setup IDE support

EOF
}

# Check if a command exists
command_exists() {
    command -v "$1" >/dev/null 2>&1
}

# Check if dotslash is installed and working
check_dotslash() {
    if command_exists dotslash; then
        print_success "dotslash is already installed"
        return 0
    else
        print_warning "dotslash is not installed"
        return 1
    fi
}

# Install dotslash
install_dotslash() {
    print_status "Installing dotslash..."
    
    local temp_dir
    temp_dir=$(mktemp -d)
    
    (
        cd "$temp_dir"
        print_status "Downloading dotslash..."
        if curl -LSfs "https://github.com/facebook/dotslash/releases/latest/download/dotslash-ubuntu-22.04.x86_64.tar.gz" | tar fxz - -C /usr/local/bin; then
            print_success "dotslash installed successfully"
        else
            print_error "Failed to download dotslash"
            return 1
        fi
    )
    
    # Clean up
    rm -rf "$temp_dir"
}

# Setup dotslash with user interaction
setup_dotslash() {
    local assume_yes="$1"
    local skip_dotslash="$2"
    
    if [[ "$skip_dotslash" = true ]]; then
        print_info "Skipping dotslash setup as requested"
        return 0
    fi
    
    print_status "Checking dotslash installation..."
    
    if check_dotslash; then
        return 0
    fi
    
    print_info "dotslash is required for Buck2 to work properly"
    print_info "This will download and install dotslash to /usr/local/bin/"
    
    if [[ "$assume_yes" = false ]]; then
        echo -n "Do you want to install dotslash? [y/N]: "
        read -r response
        case "$response" in
            [yY][eE][sS]|[yY])
                ;;
            *)
                print_warning "Skipping dotslash installation"
                print_warning "You may need to install dotslash manually for Buck2 to work"
                return 1
                ;;
        esac
    fi
    
    if install_dotslash; then
        return 0
    else
        print_error "Failed to install dotslash"
        print_info "You can install it manually with:"
        echo "  curl -L https://github.com/facebook/dotslash/releases/latest/download/dotslash-linux.tar.xz | tar -xJ"
        echo "  sudo install dotslash /usr/local/bin/"
        return 1
    fi
}

# Generate rust-project.json
setup_rust_project() {
    local skip_rust_project="$1"
    
    if [[ "$skip_rust_project" = true ]]; then
        print_info "Skipping rust-project.json generation as requested"
        return 0
    fi
    
    print_status "Setting up Rust-Analyzer IDE support..."
    
    ./buck/bin/rust-project develop //crates/... --prefer-rustup-managed-toolchain --buck2-command="./buck2"
}

# Test Buck2 functionality
test_buck2() {
    local buck2_bin="$PROJECT_ROOT/buck2"
    
    print_status "Testing Buck2 functionality..."
    
    if [[ ! -x "$buck2_bin" ]]; then
        print_error "Buck2 binary not found at $buck2_bin"
        return 1
    fi
    
    cd "$PROJECT_ROOT"
    
    print_status "Running test build: buck2 build //crates/rue:rue"
    if "$buck2_bin" build //crates/rue:rue >/dev/null 2>&1; then
        print_success "Buck2 build test passed!"
        return 0
    else
        print_warning "Buck2 build test failed"
        print_info "This might be normal if you haven't set up dependencies yet"
        print_info "Try running 'scripts/build.sh' to see detailed output"
        return 1
    fi
}

# Show next steps
show_next_steps() {
    echo ""
    echo -e "${GREEN}🎉 Development environment setup complete!${NC}"
    echo ""
    echo -e "${BLUE}Next steps:${NC}"
    echo ""
    echo -e "${YELLOW}1. Verify your setup:${NC}"
    echo "   scripts/build.sh --help     # Check build script"
    echo "   scripts/test.sh --help      # Check test script"
    echo "   scripts/run.sh --help       # Check run script"
    echo ""
    echo -e "${YELLOW}2. Try building the project:${NC}"
    echo "   scripts/build.sh            # Build the main rue compiler"
    echo "   scripts/test.sh corpus      # Run corpus tests"
    echo ""
    echo -e "${YELLOW}3. Try running the compiler:${NC}"
    echo "   scripts/run.sh -- --help    # Show compiler help"
    echo "   scripts/run.sh -- examples/basic/simple.rue  # Compile an example"
    echo ""
    echo -e "${YELLOW}4. IDE Support:${NC}"
    echo "   - Open the project in VS Code or your preferred Rust IDE"
    echo "   - Rust-Analyzer should now work with Buck2-generated targets"
    echo "   - If IDE support isn't working, try restarting your editor"
    echo ""
    echo -e "${YELLOW}5. Read the documentation:${NC}"
    echo "   - docs/spec.md              # Language specification"
    echo "   - docs/implementation.md    # Implementation details"
    echo "   - CONTRIBUTING.md           # Development workflow"
    echo "   - CLAUDE.md                 # AI assistant guidance"
    echo ""
    echo -e "${YELLOW}6. Development workflow:${NC}"
    echo "   - Use jj for version control (not git)"
    echo "   - Buck2 is the primary build system, Cargo is fallback"
    echo "   - Run tests frequently: scripts/test.sh"
    echo "   - Keep documentation updated"
    echo ""
    echo -e "${BLUE}Happy coding! 🦀${NC}"
    echo ""
}

main() {
    local assume_yes=false
    local skip_dotslash=false
    local skip_rust_project=false
    local check_only=false
    
    # Parse arguments
    while [[ $# -gt 0 ]]; do
        case $1 in
            -h|--help)
                show_help
                exit 0
                ;;
            -y|--yes)
                assume_yes=true
                shift
                ;;
            --skip-dotslash)
                skip_dotslash=true
                shift
                ;;
            --skip-rust-project)
                skip_rust_project=true
                shift
                ;;
            --check-only)
                check_only=true
                shift
                ;;
            -*)
                print_error "Unknown option: $1"
                echo "Run '$0 --help' for usage information."
                exit 1
                ;;
            *)
                print_error "Unexpected argument: $1"
                echo "Run '$0 --help' for usage information."
                exit 1
                ;;
        esac
    done

    print_success "🚀 Starting Rue development environment setup..."
    print_info "Project root: $PROJECT_ROOT"
    
    if [[ "$check_only" = true ]]; then
        print_status "Running in check-only mode..."
        
        # Check dotslash
        if check_dotslash; then
            print_success "dotslash: OK"
        else
            print_error "dotslash: MISSING"
        fi
        
        # Check rust-project.json
        if [[ -f "$PROJECT_ROOT/rust-project.json" ]]; then
            print_success "rust-project.json: EXISTS"
        else
            print_warning "rust-project.json: MISSING"
        fi
        
        # Check Buck2
        if [[ -x "$PROJECT_ROOT/buck2" ]]; then
            print_success "Buck2 binary: EXISTS"
        else
            print_error "Buck2 binary: MISSING"
        fi
        
        exit 0
    fi

    local setup_failed=false

    # Setup dotslash
    if ! setup_dotslash "$assume_yes" "$skip_dotslash"; then
        setup_failed=true
    fi

    # Setup rust-project.json
    if ! setup_rust_project "$skip_rust_project"; then
        setup_failed=true
    fi

    # Test Buck2 (non-fatal)
    test_buck2 || true

    if [[ "$setup_failed" = true ]]; then
        print_warning "Some setup steps failed, but you can continue development"
        print_info "Check the error messages above and run this script again if needed"
    else
        print_success "All setup steps completed successfully!"
    fi

    show_next_steps
}

main "$@"