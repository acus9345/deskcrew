# Throwaway windows for the demo video: a sketch pad and a note. Nothing they get is saved.
param([ValidateSet('sketch', 'notes', 'backdrop')][string]$Kind, [int]$X = 40, [int]$Y = 40, [int]$W = 560, [int]$H = 420)
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
[System.Windows.Forms.Application]::EnableVisualStyles()
$form = New-Object System.Windows.Forms.Form
$form.StartPosition = 'Manual'
$form.Location = New-Object System.Drawing.Point($X, $Y)
$form.Size = New-Object System.Drawing.Size($W, $H)
$form.Font = New-Object System.Drawing.Font('Segoe UI', 13)
$form.BackColor = [System.Drawing.Color]::White

if ($Kind -eq 'backdrop') {
    # A plain surface behind the demo windows, so nothing else on the screen gets recorded.
    $form.Text = 'Backdrop'
    $form.FormBorderStyle = 'None'
    $form.ShowInTaskbar = $false
    $form.BackColor = [System.Drawing.Color]::FromArgb(236, 234, 228)
} elseif ($Kind -eq 'sketch') {
    $form.Text = 'Sketch'
    $canvas = New-Object System.Windows.Forms.Panel
    $canvas.Dock = 'Fill'
    $canvas.AccessibleName = 'Canvas'
    $canvas.BackColor = [System.Drawing.Color]::White
    $picture = New-Object System.Drawing.Bitmap(1600, 1200)
    $g = [System.Drawing.Graphics]::FromImage($picture)
    $g.SmoothingMode = 'AntiAlias'
    $g.Clear([System.Drawing.Color]::White)
    $canvas.BackgroundImage = $picture
    $canvas.BackgroundImageLayout = 'None'
    $canvas.GetType().GetProperty('DoubleBuffered', [System.Reflection.BindingFlags]'NonPublic,Instance').SetValue($canvas, $true, $null)
    $pen = New-Object System.Drawing.Pen([System.Drawing.Color]::FromArgb(40, 40, 46), 5)
    $pen.StartCap = 'Round'; $pen.EndCap = 'Round'
    $script:last = $null
    $canvas.Add_MouseDown({ param($s, $e) $script:last = $e.Location })
    $canvas.Add_MouseMove({
        param($s, $e)
        if ($script:last) {
            $g.DrawLine($pen, $script:last, $e.Location)
            $script:last = $e.Location
            $canvas.Invalidate()
        }
    })
    $canvas.Add_MouseUp({ $script:last = $null })
    $form.Controls.Add($canvas)
} else {
    $form.Text = 'Note'
    $box = New-Object System.Windows.Forms.TextBox
    $box.Multiline = $true
    $box.Dock = 'Fill'
    $box.BorderStyle = 'None'
    $box.AccessibleName = 'Note'
    $box.Font = New-Object System.Drawing.Font('Segoe UI', 18)
    $form.Controls.Add($box)
}
[System.Windows.Forms.Application]::Run($form)
