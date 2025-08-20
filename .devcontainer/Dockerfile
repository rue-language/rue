FROM node:22

ARG TZ
ENV TZ="$TZ"

ARG CLAUDE_CODE_VERSION=latest

# Install basic development tools and iptables/ipset
RUN apt-get update && apt-get install -y --no-install-recommends \
  less \
  git \
  procps \
  sudo \
  fzf \
  zsh \
  man-db \
  unzip \
  gnupg2 \
  gh \
  iptables \
  ipset \
  iproute2 \
  dnsutils \
  aggregate \
  jq \
  nano \
  vim \
  zstd \
  build-essential \
  software-properties-common \
  wget \
  gnupg \
  gdb \
  && apt-get clean && rm -rf /var/lib/apt/lists/*

# Add the LLVM APT repository key
RUN wget -O - https://apt.llvm.org/llvm-snapshot.gpg.key | apt-key add -

# Add the LLVM APT repository for Clang 20
RUN echo "deb http://apt.llvm.org/bookworm/ llvm-toolchain-bookworm-20 main" >> /etc/apt/sources.list.d/llvm.list

# Update package lists again after adding the repository
RUN apt-get update

# Install Clang 20 and related tools
RUN apt-get install -y --no-install-recommends \
  clang-20 \
  lldb-20 \
  lld-20 \
  clang-format-20 \
  clang-tidy-20 \
  clangd-20 \
  && rm -rf /var/lib/apt/lists/*

RUN update-alternatives --install /usr/bin/clang clang /usr/bin/clang-20 100
RUN update-alternatives --install /usr/bin/clang++ clang++ /usr/bin/clang++-20 100
RUN update-alternatives --install /usr/bin/lld lld /usr/bin/lld-20 100

# Ensure default node user has access to /usr/local/share
RUN mkdir -p /usr/local/share/npm-global && \
  chown -R node:node /usr/local/share

ARG USERNAME=node

# Persist bash history.
RUN SNIPPET="export PROMPT_COMMAND='history -a' && export HISTFILE=/commandhistory/.bash_history" \
  && mkdir /commandhistory \
  && touch /commandhistory/.bash_history \
  && chown -R $USERNAME /commandhistory

# Set `DEVCONTAINER` environment variable to help with orientation
ENV DEVCONTAINER=true

# Create workspace and config directories and set permissions
RUN mkdir -p /workspace /home/node/.claude && \
  chown -R node:node /workspace /home/node/.claude

WORKDIR /workspace

# Set up non-root user
USER node

# Install global packages
ENV NPM_CONFIG_PREFIX=/usr/local/share/npm-global
ENV PATH=$PATH:/usr/local/share/npm-global/bin

# Set the default editor and visual
ENV EDITOR vim
ENV VISUAL vim

# Install Claude
RUN npm install -g @anthropic-ai/claude-code@${CLAUDE_CODE_VERSION}

# Install Rust for the node user
USER $USERNAME
ENV CARGO_HOME=/home/$USERNAME/.cargo
ENV RUSTUP_HOME=/home/$USERNAME/.rustup
ENV PATH="$CARGO_HOME/bin:$PATH"

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --default-toolchain stable \
    && rustup component add clippy rustfmt

# Install jj
RUN cargo install --locked jj-cli

# Install reindeer
RUN cargo install --locked --git https://github.com/facebookincubator/reindeer reindeer

# Install Buck2 (prebuilt binary)
USER root
RUN curl -LO https://github.com/facebook/buck2/releases/download/latest/buck2-x86_64-unknown-linux-gnu.zst \
    && zstd -d buck2-x86_64-unknown-linux-gnu.zst -o /usr/local/bin/buck2 \
    && chmod +x /usr/local/bin/buck2 \
    && rm buck2-x86_64-unknown-linux-gnu.zst

# Switch back to node for dev work
USER $USERNAME

# Copy and set up firewall script
COPY init-firewall.sh /usr/local/bin/
USER root
RUN chmod +x /usr/local/bin/init-firewall.sh && \
  echo "node ALL=(root) NOPASSWD: /usr/local/bin/init-firewall.sh" > /etc/sudoers.d/node-firewall && \
  chmod 0440 /etc/sudoers.d/node-firewall
USER node