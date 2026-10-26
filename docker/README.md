# Pmman Network 主控容器

主控镜像必须在本地 WSL 使用 `x86_64-unknown-linux-musl` 构建，生产主机只运行已经构建好的镜像。

```powershell
./scripts/build-master-image.ps1 -Version 0.4.0
```

生产机的 `docker-compose.yml` 管理主控和 PostgreSQL。节点不进入 Compose，仍由节点宿主机上的项目专属 systemd 服务运行。

生产目录和密码由部署脚本创建，不能将真实 `.env`、数据库密码、管理员密码或发布私钥提交到仓库。
