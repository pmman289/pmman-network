$ErrorActionPreference = 'Stop'
# WSL is the local build environment. Runtime and acceptance moved to txgz;
# the old shared-interface/HTTP-node laboratory is incompatible with this model.
throw 'The WSL runtime lab is retired. Build locally, then run scripts/deploy-txgz.ps1 and scripts/verify-txgz.ps1 for public master + UDP-only node acceptance.'
