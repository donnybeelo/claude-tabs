& {
    $ErrorActionPreference = 'Stop'

    $dir = Join-Path $env:LOCALAPPDATA 'Programs\claude-tabs'
    $exe = Join-Path $dir 'claude-tabs.exe'
    $url = 'https://github.com/donnybeelo/claude-tabs/releases/latest/download/claude-tabs-x86_64-pc-windows-msvc.exe'

    New-Item -ItemType Directory -Force -Path $dir | Out-Null

    Write-Host 'Downloading claude-tabs...'
    try {
        Invoke-WebRequest -Uri $url -OutFile $exe -UseBasicParsing
    } catch [System.IO.IOException] {
        Write-Host 'claude-tabs is running. Close it, then run this again to update.' -ForegroundColor Red
        return
    }

    $path = [Environment]::GetEnvironmentVariable('Path', 'User')
    if (-not $path) { $path = '' }
    if (($path -split ';') -notcontains $dir) {
        $parts = @($path.TrimEnd(';'), $dir) | Where-Object { $_ }
        [Environment]::SetEnvironmentVariable('Path', ($parts -join ';'), 'User')
        $env:Path = "$env:Path;$dir"
        Write-Host "Added $dir to your PATH"
    }

    Write-Host 'Installed. Run claude-tabs in a new terminal.' -ForegroundColor Green
}
