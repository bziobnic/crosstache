$ErrorActionPreference = 'Stop'

Write-Error @"
This legacy script is disabled because it granted an unused managed identity
subscription-wide RBAC administration. Use the supported installer instead:

    python -m installer install

The installer uses a scoped, conditioned service-principal assignment and does
not create an unused Function App managed identity.
"@
exit 1
