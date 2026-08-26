param(
    [ValidateSet("Debug", "Release")]
    [string]$Configuration = "Debug"
)

$ErrorActionPreference = "Stop"
$nativeRoot = Join-Path $PSScriptRoot "native"
$cargo = Get-Command cargo -ErrorAction SilentlyContinue
if (-not $cargo) {
    $cargoPath = Join-Path $env:USERPROFILE ".cargo\bin\cargo.exe"
    if (-not (Test-Path $cargoPath)) {
        throw "Cargo was not found. Install the Rust toolchain from https://rustup.rs/."
    }
    $cargoExecutable = $cargoPath
} else {
    $cargoExecutable = $cargo.Source
}

if (-not (Get-Command link.exe -ErrorAction SilentlyContinue)) {
    $visualStudioRoots = @(
        (Join-Path $env:ProgramFiles "Microsoft Visual Studio"),
        (Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio")
    ) | Where-Object { Test-Path $_ }

    $link = $visualStudioRoots |
        ForEach-Object {
            Get-ChildItem $_ -Filter link.exe -File -Recurse -ErrorAction SilentlyContinue |
                Where-Object { $_.FullName -match "\\VC\\Tools\\MSVC\\[^\\]+\\bin\\Hostx64\\x64\\link\.exe$" }
        } |
        Sort-Object FullName -Descending |
        Select-Object -First 1

    if (-not $link) {
        throw "The MSVC x64 linker was not found. Install Visual Studio Build Tools with Desktop development with C++."
    }

    $toolRoot = $link.Directory.Parent.Parent.Parent.FullName
    $includePaths = @((Join-Path $toolRoot "include"))
    $libraryPaths = @(
        (Join-Path $toolRoot "lib\x64"),
        (Join-Path $toolRoot "lib\onecore\x64")
    )

    $windowsKitsRoot = Join-Path ${env:ProgramFiles(x86)} "Windows Kits\10"
    $kernelLibrary = if (Test-Path $windowsKitsRoot) {
        Get-ChildItem (Join-Path $windowsKitsRoot "Lib") -Filter kernel32.lib -File -Recurse -ErrorAction SilentlyContinue |
            Where-Object { $_.FullName -match "\\um\\x64\\kernel32\.lib$" } |
            Sort-Object FullName -Descending |
            Select-Object -First 1
    }

    if ($kernelLibrary) {
        $sdkVersion = $kernelLibrary.Directory.Parent.Parent.Name
        $sdkInclude = Join-Path $windowsKitsRoot "Include\$sdkVersion"
        $sdkLib = Join-Path $windowsKitsRoot "Lib\$sdkVersion"
        $includePaths += @(
            (Join-Path $sdkInclude "ucrt"),
            (Join-Path $sdkInclude "shared"),
            (Join-Path $sdkInclude "um"),
            (Join-Path $sdkInclude "winrt")
        )
        $libraryPaths += @(
            (Join-Path $sdkLib "ucrt\x64"),
            (Join-Path $sdkLib "um\x64")
        )
    } else {
        $scopeKernel = $visualStudioRoots |
            ForEach-Object {
                Get-ChildItem $_ -Filter kernel32.lib -File -Recurse -ErrorAction SilentlyContinue |
                    Where-Object { $_.FullName -match "\\SDK\\ScopeCppSDK\\vc15\\SDK\\lib\\kernel32\.lib$" }
            } |
            Sort-Object FullName -Descending |
            Select-Object -First 1

        if (-not $scopeKernel) {
            throw "A Windows SDK containing kernel32.lib was not found."
        }

        $scopeSdk = $scopeKernel.Directory.Parent.FullName
        $scopeRoot = $scopeSdk | Split-Path
        $includePaths += @(
            (Join-Path $scopeSdk "include"),
            (Join-Path $scopeSdk "include\ucrt"),
            (Join-Path $scopeRoot "VC\include")
        )
        $libraryPaths += @(
            (Join-Path $scopeSdk "lib"),
            (Join-Path $scopeRoot "VC\lib")
        )
    }

    $env:PATH = "$($link.Directory.FullName);$env:PATH"
    $env:INCLUDE = (($includePaths | Where-Object { Test-Path $_ }) -join ";")
    $env:LIB = (($libraryPaths | Where-Object { Test-Path $_ }) -join ";")
}

$arguments = @("build", "--locked", "--manifest-path", (Join-Path $nativeRoot "Cargo.toml"))
if ($Configuration -eq "Release") {
    $arguments += "--release"
}

& $cargoExecutable @arguments
if ($LASTEXITCODE -ne 0) {
    exit $LASTEXITCODE
}
