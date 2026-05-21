using namespace WEX.TestExecution
using namespace WEX.TestExecution.PowerShell
using namespace WEX.Logging.Interop
Set-StrictMode -Version 3
$errorActionPreference = 'Stop'
$script:LinuxVhdGen2 = (Join-Path -Path (Get-Location) -ChildPath "\depcache\ImageStore\VHDX\Common\Linux\Ubuntu\amd64\Ubuntu_Latest.vhdx")
$script:LinuxVhdGen1 = (Join-Path -Path (Get-Location) -ChildPath "\depcache\ImageStore\VHDX\Common\Linux\Ubuntu\amd64\Ubuntu1604_Gen1.vhdx")
$script:LinuxIso = (Join-Path -Path (Get-Location) -ChildPath "\alpine-standard-x86_64.iso")
$script:TestIso = (Join-Path -Path (Get-Location) -ChildPath "\testimg.iso")
$script:UnderhillBin = (Join-Path -Path (Get-Location) -ChildPath "\underhill.bin")
$script:FirmwareParameter = ""
$script:FirmwareParameterEnableDebug = "RUST_BACKTRACE=full HVLITE_LOG=info,storvsp=debug,scsidisk=debug"
$script:Version = "11.1"
$script:NvmeEmulatorId = '{3217a48a-c820-4727-8c4c-3e2086aba839}'
$script:ND2_EMULATOR_ID = '{9a1714e8-4490-4d97-b4d8-e80888a7992e}'
$script:VirtualSerialNumber = "0322AIB1Q0450I09053G"
$script:SwitchName = ""
$script:Cleanup = $true
$script:SetupTestVMRoot = $true
$script:VHDSKU = 'ServerStandardCore'
$script:ToolsFolder = Join-Path -Path (Get-Location) -ChildPath "\tools"
$script:Vtl2ScsiController0 = "0edba88a-486b-40c8-b0b6-679e9ad6a4d1"
$script:Vtl2ScsiController1 = "0edba88b-486b-40c8-b0b6-679e9ad6a4d1"
$script:Vtl2ScsiController2 = "0edba88c-486b-40c8-b0b6-679e9ad6a4d1"
$script:Vtl2ScsiController3 = "0edba88d-486b-40c8-b0b6-679e9ad6a4d1"
$script:ComplianceTestFolder = ".\ScsiCompliance"
function ModuleSetup
{
    [ModuleSetup()]
    param()
    if ($script:Cleanup)
    {
        CleanupVMs
        CleanupVhdx
        if (Test-Path $script:ToolsFolder)
        {
            Get-ChildItem -Path $script:ToolsFolder -Recurse -ErrorAction Ignore | Remove-Item -force -Recurse -ErrorAction Ignore
        }
    }
    if ($script:SetupTestVMRoot)
    {
        $testVmRoot = Get-Location
        [Log]::Comment("Set TestVm root: " + $testVmRoot)
        Set-TestVmRoot -Path $testVmRoot
    }
    # If no external switch exists (DES), create a switch
    if (-not (Get-VMSwitch -SwitchType External))
    {
        Get-VMSwitch -Name "TestInternalSwitch" -ErrorAction Ignore | remove-vmswitch -force
        $InternalSwitch = New-VMSwitch -SwitchType Internal "TestInternalSwitch"
        $script:SwitchName = $InternalSwitch.Name
    }
    else
    {
        $script:SwitchName = (Get-TestDefaultExternalSwitch).Name
    }
    [Log]::Comment("Get VM switch: " + $script:SwitchName)
    if (-not (Test-Path $script:ToolsFolder))
    {
        New-Item $script:ToolsFolder -ItemType Directory
    }
    [Log]::Comment("Copy test binaries...")
    Copy-TestItem -Chunk "test_automation_bins" -ChildPath atlascloud\components\logger\minwin\wttlog.dll -Destination $script:ToolsFolder
    Copy-TestItem -Chunk "test_automation_bins" -ChildPath storage\tests\scsicompliance\scsicompliance.exe -Destination $script:ToolsFolder
    Copy-TestItem -Chunk "test_automation_bins" -ChildPath vm\tools\UpdateDriverForDevice.exe -Destination $script:ToolsFolder
}
function TestCleanup
{
    [TestCleanup()]
    param()
    Set-PowerTestSpecialMode -Mode None
    if ($script:Cleanup)
    {
        CleanupVMs
        CleanupVhdx
    }
}
function CleanupVhdx {
    param()
    $vhdxList = Get-ChildItem * -Include "*TestItem*" "*.vhdx"
    foreach($vhdxFile in $vhdxList)
    {
        [Log]::Comment("Removing " + $vhdxFile.Name)
        $vhdxFile | Remove-Item
    }
}
function CleanupVMs {
    param ()
    $vms = @(Get-VM)
    foreach ($vm in $vms)
    {
        [Log]::Comment("Cleanup VM: " + $vm.Name)
        $vmPath = $vm.Path
        $vm | Stop-VM -Force -TurnOff -ErrorAction Ignore
        $vm | Remove-VM -Force -ErrorAction Ignore
        Get-ChildItem -Path $vmPath -Recurse -ErrorAction Ignore | Remove-Item -force -Recurse -ErrorAction Ignore
    }
    Remove-UnderhillLogJobs
    $testVmRoot = Get-TestVmRoot
    [Log]::Comment("Cleanup folder: " + $testVmRoot)
    Get-ChildItem -Path $testVmRoot -Recurse -ErrorAction Ignore | Remove-Item -force -Recurse -ErrorAction Ignore
}
function New-NVMeVm {
    param (
        [Parameter(Mandatory = $true)]
        [string]$Name,
        [ValidateSet(1, 2)]
        [int] $Generation = 2,
        [ValidateNotNullOrEmpty()]
        [string] $FirmwareFilePath,
        [string] $FirmwareParameter,
        [string] $Version = $script:Version,
        [bool] $LinuxOS = $false
    )
    Set-PowerTestSpecialMode -Mode Underhill -PrivateFirmware $FirmwareFilePath
    [string] $IsolationType = "TrustedLaunch"
    if ($Generation -eq 1)
    {
        $IsolationType = "None"
    }
    if ($LinuxOS)
    {
        if($Generation -eq 2)
        {
            New-LinuxTestVM -Name $Name -SwitchName $script:SwitchName -Generation $Generation -GuestStateIsolation $IsolationType -Version $Version -VhdPath $script:LinuxVhdGen2
        }
        else
        {
            New-LinuxTestVM -Name $Name -SwitchName $script:SwitchName -Generation $Generation -GuestStateIsolation $IsolationType -Version $Version -VhdPath $script:LinuxVhdGen1
        }
    }
    else
    {
        New-TestVM -Name $Name -SwitchName $script:SwitchName -Generation $Generation -GuestStateIsolation $IsolationType -Version $Version -VHDSKU $Script:VHDSKU
    }
    if (-not [string]::IsNullOrWhiteSpace($FirmwareParameter))
    {
        Set-VMFirmwareParameters -Name $Name -CommandLine $FirmwareParameter
    }
    Set-VM $Name -AutomaticStopAction TurnOff
    Add-NVMeDevice -VmName $Name -Generation $Generation
    Enable-UnderhillCom3LogFile -Name $Name
}
function Reset-VmHostNVMeDevices {
    param ()
    Reset-NVMeDevicesDriver
    Reset-VmHostAssignableDevices
}
function Reset-NVMeDevicesDriver {
    param (
        [string]$DeviceId
    )
    if (-not [string]::IsNullOrWhiteSpace($DeviceId))
    {
        $devices = @(& "$script:ToolsFolder\UpdateDriverForDevice.exe" "-listall" | findstr $DeviceId)
    }
    else
    {
        $devices = @(& "$script:ToolsFolder\UpdateDriverForDevice.exe" "-listall")
    }
    foreach ($devInfo in $devices)
    {
        [Log]::Comment("devInfo: " + $devInfo)
        $data = $devInfo.Split(" ",[System.StringSplitOptions]::RemoveEmptyEntries)
        if ($data -is [array] -and $data.count -gt 4 -and ($data[0] -eq "vmnvmed" -or $data[0] -eq "vmnvmed2"))
        {
            [Log]::Comment("Remove NVMe Direct driver for device: " + $data[2])
            try {
                $null = & "$script:ToolsFolder\UpdateDriverForDevice.exe" "-prefix" "PCI" $data[2]
            }
            catch {
                [Log]::Comment("UpdateDriverForDevice.exe: " + $_.Exception.Message)
            }
        }
    }
}
function Install-NVMeDevicesDriver {
    param (
            [string]$DeviceId
        )
    if (-not [string]::IsNullOrWhiteSpace($DeviceId))
    {
        $devices = @(& "$script:ToolsFolder\UpdateDriverForDevice.exe" "-listall" | findstr $DeviceId)
    }
    else
    {
        $devices = @(& "$script:ToolsFolder\UpdateDriverForDevice.exe" "-listall")
    }
    $installed = $false
    $deviceLocation = $null
    foreach ($devInfo in $devices)
    {
        [Log]::Comment("devInfo: " + $devInfo)
        $data = $devInfo.Split(" ",[System.StringSplitOptions]::RemoveEmptyEntries)
        if ($data -is [array] -and $data.count -gt 4 -and $data[0] -eq "stornvme" -and $data[3] -ne "--------" -and $data[3] -ne "0")
        {
            [Log]::Comment("Install NVMe Direct driver for device: " + $data[2])
            try {
                $null = & "$script:ToolsFolder\UpdateDriverForDevice.exe" "-prefix" "NVMD2" $data[2]
            }
            catch {
                [Log]::Comment("Install NVMe Direct driver failed: " + $_.Exception.Message)
            }
            $newDevInfo = & "$script:ToolsFolder\UpdateDriverForDevice.exe" "-listall" | findstr $data[2]
            [Log]::Comment("newDevInfo: " + $newDevInfo)
            if (-not [string]::IsNullOrWhiteSpace($newDevInfo) -and $newDevInfo.Split(" ",[System.StringSplitOptions]::RemoveEmptyEntries)[1] -eq "OK")
            {
                $newData = $newDevInfo.Split(" ",[System.StringSplitOptions]::RemoveEmptyEntries)
                if ($newData -is [array] -and $newData.count -gt 4 -and $newData[0] -eq "vmnvmed2" -and $newData[1] -eq "OK")
                {
                    $installed = $true
                    $deviceLocation = $data[2]
                    break
                }
                else
                {
                    [Log]::Comment("NVMe Direct driver doesn't work on device: " + $data[2])
                    try {
                        $null = & "$script:ToolsFolder\UpdateDriverForDevice.exe" "-prefix" "PCI" $data[2]
                    }
                    catch {
                        [Log]::Comment("UpdateDriverForDevice.exe: " + $_.Exception.Message)
                    }
                }
            }
        }
    }
    
    return @{"Installed"= $installed; "DeviceLocation"= $deviceLocation}
}
function Get-NVMeCapableDevices {
    param ()
    $ddaCapableDevices = @(Get-DDACapableDevices)
    $nvmeCapableDevices = @()
    foreach ($devInfo in $ddaCapableDevices)
    {
        [Log]::Comment("devInfo: " + $devInfo)
        if ($devInfo.Devices -is [system.array] -and ($devInfo.Devices.count -eq 32 -or $devInfo.Devices.count -eq 33))
        {
            [Log]::Comment("Devices count: " + $devInfo.Devices.count)
            foreach ($device in $devInfo.Devices)
            {
                [Log]::Comment("device: " + $device)
                $deviceStatus = Install-NVMeDevicesDriver -DeviceId $device.DeviceID
                if ($deviceStatus.Installed)
                {
                    [Log]::Comment("Get NVMe capable device: " + $device.DeviceID)
                    $nvmeCapableDevices += @{"Location:" = $deviceStatus.DeviceLocation; "DeviceId" = $device.DeviceId}
                }
            }
        }
    }
    return $nvmeCapableDevices
}
function Add-RPBDevice {
    param (
            [Parameter(Mandatory = $true)]
            [string]$VmName,
            [int]$ControllerIndex = 0,
            [int]$Location = 1,
            [int]$Channel = 0,
            [int]$NsId = 1,
            [bool]$NVMeDirect = $false,
            [ValidateSet(1, 2)]
            [int] $Generation = 2
        )
    $vm = get-vm $VmName
    <#
    if ($NVMeDirect)
    {
        $nvmeCapableDevices = @(Get-NVMeCapableDevices)
        if ($nvmeCapableDevices.count -gt 0)
        {
            $device = $nvmeCapableDevices[0]
            [Log]::Comment("Selected device: " + $device.DeviceID)
            $emulatorConfig = New-Nd2EmulatorConfig -Bdf $device.Location -VirtualSerialNumber "serial_0001"
            Write-Host("emulator config: " + $emulatorConfig)
            $vm | Add-FlexIoDevice -EmulatorId $script:ND2_EMULATOR_ID -ConfigurationStrings $emulatorConfig -TargetVtl 2
            $flexIoDevices = ($vm | Get-FlexIoSettingData)
            foreach ($devices in $flexIoDevices)
            {
                if(($devices.EmulatorConfiguration -join ";") -eq ($emulatorConfig -join ";"))
                {
                    $vsids = $devices.VirtualSystemIdentifiers
                }
            }
        }
        else
        {
            [Log]::Comment("There are no NVMeD2 capable devices on this test machine. Using NVMe Emulator.")
            Test-NvmeEmulator
            $vhdPath = Join-Path -Path ($vm.Path) -ChildPath "$VmName-TestVHD-$ControllerIndex-$Location.vhdx"
            New-NvmeVhdx -Path $vhdPath -VM $vm -VHDSize 1GB
            [Log]::Comment("Adding FlexIoDevice: " + $vhdPath + " to VM: " + $VmName)
            $vm | Add-FlexIoDevice -EmulatorId $script:NvmeEmulatorId -ConfigurationStrings @($vhdPath) -TargetVtl 2
            $vsids = ($vm | Get-FlexIoSettingData | Where-Object EmulatorConfiguration -eq $vhdPath).VirtualSystemIdentifiers
        }
    }
    else
    #>
    {
        $ddaCapableDevices = @(Get-DDACapableDevices)
        if ($ddaCapableDevices.count -gt 0)
        {
            $device = $ddaCapableDevices[0].Devices[0]
            [Log]::Comment("Selected device: " + $device.DeviceID)
            $locationpath = ($device | Get-PnpDeviceProperty DEVPKEY_Device_LocationPaths).data[0]
            [Log]::Comment("Enable DDA capable device: " + $($device.PNPDeviceID))
            Enable-DDACapableDeviceForVmAssignment -DDACapableDevice $ddaCapableDevices[0]
            Add-VMAssignableDevice -VMName $VmName -LocationPath $locationpath
            Set-VmAssignableDevice -VMName $VmName -LocationPath $locationPath -TargetVtl 2
            $instanceId = (Get-VMAssignableDevice -VMName $VmName).Id
            $vsids = (get-ciminstance Msvm_PciExpressSettingData -namespace root/virtualization/v2 | Where-Object InstanceID -eq $instanceId).VirtualSystemIdentifiers
         }
         else
         {
            [Log]::Comment("There are no DDA capable devices on this test machine. Using NVMe Emulator.")
            Test-NvmeEmulator
            $vhdPath = Join-Path -Path ($vm.Path) -ChildPath "$VmName-TestVHD-$ControllerIndex-$Location.vhdx"
            New-NvmeVhdx -Path $vhdPath -VM $vm -VHDSize 1GB
            [Log]::Comment("Adding HardDiskDrive: " + $vhdPath + " to VM: " + $VmName)
            Add-VmNvmeEmulator -VmName $VmName -VhdPath $vhdPath -TargetVtl 2
            $vsids = (Get-VmNvmeEmulator $VmName | Where-Object EmulatorConfiguration -eq $vhdPath).VirtualSystemIdentifiers
        }
    }
    $vsid = $vsids[0].ToString().trim("{}")
    $singleDevice = [PSCustomObject]@{
        device_type     = 'nic'
        device_path     = $vsid
        sub_device_path = $NsId
    }
    $physicalDevices = [PSCustomObject]@{
        type   = 'single'
        device = $singleDevice
    }
    $lun = [PSCustomObject]@{
        channel                = $Channel
        location               = $Location
        device_id              = [Guid]::NewGuid()
        vendor_id              = "RPB"
        product_id             = "RPB"
        product_revision_level = "1.0"
        serial_number          = $script:VirtualSerialNumber
        model_number           = "OPQRSTUVWXYZ"
        physical_devices       = $physicalDevices
    }
    $guestManagement = Get-CimInstance -Namespace "root\virtualization\v2" Msvm_VirtualSystemGuestManagementService
    $result = $guestManagement | Invoke-CimMethod -MethodName GetManagementVtlSettings -Arguments @{"VmId" = $Vm.Id.Guid; "Namespace"="NetworkDevice"} |
                                Trace-CimMethodExecution -CimInstance $guestManagement -MethodName "GetManagementVtlSettings"
    # No settings, create w/ defaults
    if ($result.Settings.Length -le 4)
    {
        $protocol = "NIC"
        [Log]::Comment("No Vtl2Settings found. Creating from scratch.")
        $luns = @()
        $luns += $lun
        $controller = [PSCustomObject]@{
            instance_id            = 'a09c8c03-cccb-47f6-b6d5-7c3ef451eb5c'
        }
        $controllerarray = @()
        $controllerarray += $controller
        $controllers = [PSCustomObject]@{
            nic_devices = $controllerarray
        }
        $vtl2settings = [PSCustomObject]@{
            version = "V1"
            dynamic = $controllers
        }
    }
    else
    {
        $oldSettings = [System.Text.Encoding]::UTF8.GetString($result.Settings[4..($result.Settings.Length - 1)])
        [Log]::Comment("Old Vtl2Settings: " + $oldSettings)
        $vtl2Settings = $oldSettings | ConvertFrom-Json
        $vtl2Settings.dynamic.nic_devices[$ControllerIndex].luns += $lun
    }
    $jsonVtl2settings = $vtl2settings | ConvertTo-Json -Depth 8 -Compress
    [Log]::Comment("New Vtl2Settings: " + $jsonVtl2settings)
    Set-Vtl2Settings -Name $vm -Settings $vtl2settings -ConvertDepth 8 -Namespace "NetworkDevice" -CurrentUpdateId $result.CurrentUpdateId
}
function Test-ScsiCompliance {
    param (
            [Parameter(Mandatory = $true)]
            [string]$VmName,
            [switch]$Iso,
            [switch]$DisableAssert
        )
    $testFolder = ".\ScsiCompliance"
    if (-not (Test-Path $testFolder)) { New-Item $testFolder -ItemType Directory }
    $logPath = "$testFolder\ScsiCompliance.$VmName.wtl"
    [Log]::Comment("Get test vm session...")
    $vmSession = Get-TestVMPSSession $VmName -PowershellDirect -Verbose
    Invoke-Command -Session $vmSession -ScriptBlock { if (-not (Test-Path C:\test)) { New-Item C:\test -ItemType Directory } }
    Copy-Item -ToSession $vmSession -Path $script:ToolsFolder\scsicompliance.exe -Destination C:\test -Force
    Copy-Item -ToSession $vmSession -Path $script:ToolsFolder\wttlog.dll -Destination C:\test -Force
    [Log]::Comment("Kick off SCSI compliance test...")
    if (-not $Iso)
    {
        Invoke-Command -Session $vmSession -ScriptBlock {
            Set-Location C:\test
            & .\scsicompliance.exe /Device \\.\PhysicalDrive1 /Operation Test /Scenario Common /Verbosity 4
        }
    }
    else
    {
        Invoke-Command -Session $vmSession -ScriptBlock {
            Set-Location C:\test
            & .\scsicompliance.exe /Device \\.\CDRom0 /Operation Test /Scenario Common /Verbosity 4
        }
    }
    
    [Log]::Comment("Retrieve test results...")
    Copy-Item "C:\test\ScsiCompliance.wtl" $logPath -FromSession $vmSession -Force
    if(-not $DisableAssert)
    {
        $wtl = [xml](Get-Content $logPath)
        Assert-Condition -Success ( $wtl.'WTT-Logger'.PFRollup.Failed -eq 0 ) -Message "At least one SCSI compliance test failed"
        Remove-Item $logPath -force
    }
    $vmSession | Remove-PSSession
}
function New-DeviceMappingVm()
{
    param (
        [ValidateNotNullOrEmpty()]
        [string] $VmName
    )
    $null = @(
        $gen = 2
        Set-PowerTestSpecialMode -Mode "underhill" -PrivateFirmware $script:UnderhillBin
        $vm = New-TestVM -Name $VmName -Generation $gen -NoVhd -Version 11.1 -SwitchName $script:SwitchName
        # Clear VTL2 settings
        $linuxTemplate = "272e7447-90a4-4563-a4b9-8e4ab00526ce"
        Set-VMFirmware -VMname $vmName -EnableSecureBoot On -SecureBootTemplateId $linuxTemplate
        [Log]::Comment("Add 2 SCSI controllers to VM: $VMName")
        $vm | Add-VMScsiController
        $controller0 = Get-VMScsiController -Vm $vm -ControllerNumber 0
        $controller1 = Get-VMScsiController -Vm $vm -ControllerNumber 1
        Set-VmScsiControllerTargetVtl -Controller $controller0 -TargetVtl 2
        Set-VmScsiControllerTargetVtl -Controller $controller1 -TargetVtl 2