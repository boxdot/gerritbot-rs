#!/bin/bash
# adapted from https://github.com/openfrontier/docker-gerrit
set -ex

BASE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)"
GERRIT_HOME="${BASE_DIR}/data/gerrit"
GERRIT_SITE="${GERRIT_HOME}/review_site"
GERRIT_WAR="${GERRIT_HOME}/gerrit.war"
GERRIT_VERSION=2.14.19

mkdir -p "${GERRIT_HOME}"

[ -e "$GERRIT_WAR" ] || \
    curl -fSsL \
    "https://gerrit-releases.storage.googleapis.com/gerrit-${GERRIT_VERSION}.war" \
    -o "$GERRIT_WAR"

# download Plugins
PLUGIN_VERSION=bazel-stable-2.14
GERRITFORGE_URL=https://gerrit-ci.gerritforge.com
GERRITFORGE_ARTIFACT_DIR=lastSuccessfulBuild/artifact/bazel-genfiles/plugins

# delete-project
[ -e "${GERRIT_HOME}/delete-project.jar" ] || \
    curl -fSsL \
    "${GERRITFORGE_URL}/job/plugin-delete-project-${PLUGIN_VERSION}/${GERRITFORGE_ARTIFACT_DIR}/delete-project/delete-project.jar" \
    -o "${GERRIT_HOME}/delete-project.jar"

mkdir -p "${GERRIT_SITE}"

# Initialize Gerrit if ${GERRIT_SITE}/git is empty.
if [ -z "$(ls -A "$GERRIT_SITE/git")" ]; then
    echo "First time initialize gerrit..."
    java -jar "${GERRIT_WAR}" init --batch --no-auto-start -d "${GERRIT_SITE}"
fi

cp -f "${GERRIT_HOME}/delete-project.jar" "${GERRIT_SITE}/plugins/delete-project.jar"

set_gerrit_config() {
    git config -f "${GERRIT_SITE}/etc/gerrit.config" "$@"
}

set_gerrit_config sendemail.enable false
set_gerrit_config plugins.allowRemoteAdmin true
set_gerrit_config sshd.listenAddress localhost:29418
set_gerrit_config httpd.listenUrl http://localhost:8080/
set_gerrit_config gerrit.canonicalWebUrl http://localhost:8080/
set_gerrit_config auth.type DEVELOPMENT_BECOME_ANY_ACCOUNT

java -jar "${GERRIT_WAR}" init --batch -d "${GERRIT_SITE}"
java -jar "${GERRIT_WAR}" reindex --verbose -d "${GERRIT_SITE}"
java -jar "${GERRIT_WAR}" daemon --console-log -d "${GERRIT_SITE}"
