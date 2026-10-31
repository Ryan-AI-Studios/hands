param([int]$HoldSeconds = 0)

# Hands 0122 scratch fixture. Self-contained WinForms window used only by the
# ignored live tests in tests/uia_action_live.rs (owner decision D-2: the test
# must drive a window it spawned itself, never one the owner left open).
#
# Controls:
#   Send     (Button)   - gated by the fence classifier (Messages)
#   Refresh  (Button)   - benign; supports Invoke but not Toggle
#   Enable   (CheckBox) - supports Toggle
#   Name     (TextBox)  - text-capable (Edit role) for set-value
#   Panel    (Panel)    - deliberately unnamed: no_element_evidence case
#   Status   (Label)    - inert output
#
# Prints `hwnd=0x...` on stdout, then pumps messages until killed (or
# -HoldSeconds). Every named control sets AccessibleName explicitly: a bare
# WinForms TextBox can expose an empty accessible name, which would refuse
# `no_element_evidence` and make the positive set-value case meaningless.

Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing

# The fixture must never take the foreground: the live test proves a background
# dispatch (DoD-3), so the window is created without activation and refuses
# activation on click.
Add-Type -ReferencedAssemblies System.Windows.Forms, System.Drawing -TypeDefinition @"
using System.Windows.Forms;

public class HandsScratchForm : Form
{
    protected override bool ShowWithoutActivation { get { return true; } }

    protected override CreateParams CreateParams
    {
        get
        {
            CreateParams cp = base.CreateParams;
            cp.ExStyle |= 0x08000000; // WS_EX_NOACTIVATE
            return cp;
        }
    }
}
"@

$form = New-Object HandsScratchForm
$form.Text = 'Hands 0122 scratch'
$form.AccessibleName = 'Hands 0122 scratch'
$form.ClientSize = New-Object System.Drawing.Size(420, 220)
$form.StartPosition = 'Manual'
$form.Left = 140
$form.Top = 140
$form.TopMost = $false

$send = New-Object System.Windows.Forms.Button
$send.Text = 'Send'
$send.AccessibleName = 'Send'
$send.Location = New-Object System.Drawing.Point(20, 20)
$send.Size = New-Object System.Drawing.Size(120, 30)

$refresh = New-Object System.Windows.Forms.Button
$refresh.Text = 'Refresh'
$refresh.AccessibleName = 'Refresh'
$refresh.Location = New-Object System.Drawing.Point(20, 60)
$refresh.Size = New-Object System.Drawing.Size(120, 30)

$enable = New-Object System.Windows.Forms.CheckBox
$enable.Text = 'Enable'
$enable.AccessibleName = 'Enable'
$enable.Location = New-Object System.Drawing.Point(20, 100)
$enable.Size = New-Object System.Drawing.Size(120, 24)

$name = New-Object System.Windows.Forms.TextBox
$name.Text = 'scratch'
$name.AccessibleName = 'Name'
$name.Location = New-Object System.Drawing.Point(20, 130)
$name.Size = New-Object System.Drawing.Size(200, 24)

$panel = New-Object System.Windows.Forms.Panel
$panel.AccessibleName = ''
$panel.Text = ''
$panel.Location = New-Object System.Drawing.Point(240, 20)
$panel.Size = New-Object System.Drawing.Size(60, 30)

$status = New-Object System.Windows.Forms.Label
$status.Text = 'idle'
$status.AccessibleName = 'Status'
$status.Location = New-Object System.Drawing.Point(20, 165)
$status.Size = New-Object System.Drawing.Size(360, 24)

$send.Add_Click({ $status.Text = 'send' })
$refresh.Add_Click({ $status.Text = 'refresh' })

$form.Controls.AddRange(@($send, $refresh, $enable, $name, $panel, $status))
[void]$form.Show()

Write-Output ("hwnd=0x{0:x}" -f $form.Handle.ToInt64())
[Console]::Out.Flush()

if ($HoldSeconds -gt 0) {
    $end = (Get-Date).AddSeconds($HoldSeconds)
    while ((Get-Date) -lt $end) {
        [System.Windows.Forms.Application]::DoEvents()
        Start-Sleep -Milliseconds 100
    }
} else {
    while ($true) {
        [System.Windows.Forms.Application]::DoEvents()
        Start-Sleep -Milliseconds 100
    }
}
