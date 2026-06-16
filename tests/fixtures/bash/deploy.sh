#!/bin/bash

# Configuration
APP_NAME="myapp"
DEPLOY_DIR="/opt/myapp"
LOG_FILE="/var/log/deploy.log"

# Function: log a message
log_message() {
    local message="$1"
    echo "[$(date)] $message" >> "$LOG_FILE"
}

# Function: check prerequisites
check_prerequisites() {
    local required_tools=("git" "docker" "kubectl")
    for tool in "${required_tools[@]}"; do
        if ! command -v "$tool" &> /dev/null; then
            log_message "ERROR: $tool not found"
            return 1
        fi
    done
    return 0
}

# Function: deploy the application
deploy() {
    local version="$1"
    local environment="$2"

    log_message "Deploying $APP_NAME v$version to $environment"

    # Build and deploy
    docker build -t "$APP_NAME:$version" .
    kubectl set image deployment/"$APP_NAME" "$APP_NAME=$APP_NAME:$version"

    log_message "Deployment complete"
}

# Function: rollback
rollback() {
    log_message "Rolling back $APP_NAME"
    kubectl rollout undo deployment/"$APP_NAME"
}

# Main
main() {
    local action="${1:-deploy}"
    local version="${2:-latest}"

    check_prerequisites || exit 1

    case "$action" in
        deploy)
            deploy "$version" "production"
            ;;
        rollback)
            rollback
            ;;
        *)
            log_message "Unknown action: $action"
            exit 1
            ;;
    esac
}

main "$@"
