# REA (morluto/rea) —— 逆向工程 CLI + MCP server
# 基于本地已有的 node:22-alpine 构建（Docker Hub 不可达）
FROM node:22-alpine

# rea 官方要求：Node ^22.19.0 || ^24.11.0 || >=26
RUN npm install --global rea-agents@latest \
 && rea --version

ENTRYPOINT []
CMD ["rea", "--help"]
