# A throwaway window for the desktop tests. It records everything that reaches it.
param([string]$Title, [string]$Log, [int]$X = 200, [int]$Y = 120)
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
[System.Windows.Forms.Application]::EnableVisualStyles()
function Note([string]$text) { [System.IO.File]::AppendAllText($Log, "$text`n") }

$form = New-Object System.Windows.Forms.Form
$form.Text = $Title
$form.StartPosition = 'Manual'
$form.Location = New-Object System.Drawing.Point($X, $Y)
$form.ClientSize = New-Object System.Drawing.Size(640, 420)

$strip = New-Object System.Windows.Forms.MenuStrip
$file = New-Object System.Windows.Forms.ToolStripMenuItem('File')
$save = New-Object System.Windows.Forms.ToolStripMenuItem('Save')
$save.Add_Click({ Note 'menu Save' })
[void]$file.DropDownItems.Add($save)
[void]$strip.Items.Add($file)
$form.Controls.Add($strip)
$form.MainMenuStrip = $strip

$status = New-Object System.Windows.Forms.Label
$status.Name = 'Status'; $status.Text = 'Ready'; $status.Location = New-Object System.Drawing.Point(10, 32); $status.AutoSize = $true
$form.Controls.Add($status)

function Button([string]$text, [int]$x, [scriptblock]$action) {
    $b = New-Object System.Windows.Forms.Button
    $b.Text = $text; $b.Location = New-Object System.Drawing.Point($x, 56); $b.Width = 84
    $b.Add_Click($action)
    $form.Controls.Add($b)
    $b
}
$script:alpha = Button 'Alpha' 10 { Note 'click Alpha'; $status.Text = 'Pressed Alpha' }
$script:beta = Button 'Beta' 100 { Note 'click Beta'; $status.Text = 'Pressed Beta' }
[void](Button 'Dialog' 190 { Note 'dialog open'; [void][System.Windows.Forms.MessageBox]::Show($form, 'Are you sure', 'Check'); Note 'dialog closed' })
[void](Button 'Rebuild' 280 {
    $form.Controls.Remove($script:beta); $script:beta.Dispose()
    $script:beta = Button 'Beta' 100 { Note 'click Beta'; $status.Text = 'Pressed Beta' }
    Note 'rebuilt'
})
[void](Button 'Vanish' 370 { $form.Controls.Remove($script:beta); Note 'vanished' })

$wrap = New-Object System.Windows.Forms.CheckBox
$wrap.Text = 'Wrap'; $wrap.Location = New-Object System.Drawing.Point(10, 90)
$wrap.Add_CheckedChanged({ Note "wrap $($wrap.Checked)" })
$form.Controls.Add($wrap)

$field = New-Object System.Windows.Forms.TextBox
$field.Name = 'Field'; $field.Text = 'start'; $field.Location = New-Object System.Drawing.Point(120, 90); $field.Width = 180
$field.Add_TextChanged({ Note "field $($field.Text)" })
$field.Add_KeyDown({ Note "key $($_.KeyCode)" })
$form.Controls.Add($field)

$combo = New-Object System.Windows.Forms.ComboBox
$combo.Name = 'Colour'; $combo.DropDownStyle = 'DropDownList'; $combo.Location = New-Object System.Drawing.Point(310, 90)
[void]$combo.Items.AddRange(@('Red', 'Green', 'Blue')); $combo.SelectedIndex = 0
$combo.Add_SelectedIndexChanged({ Note "colour $($combo.SelectedItem)" })
$form.Controls.Add($combo)

$rows = New-Object System.Windows.Forms.ListBox
$rows.Name = 'Rows'; $rows.Location = New-Object System.Drawing.Point(440, 90); $rows.Size = New-Object System.Drawing.Size(180, 80)
foreach ($i in 1..30) { [void]$rows.Items.Add("Row $i") }
$rows.Add_SelectedIndexChanged({ Note "row $($rows.SelectedItem)" })
$form.Controls.Add($rows)

# A canvas: no accessibility actions at all, only mouse events.
$pad = New-Object System.Windows.Forms.Panel
$pad.Name = 'Pad'; $pad.BackColor = [System.Drawing.Color]::FromArgb(250, 240, 0)
$pad.Location = New-Object System.Drawing.Point(10, 180); $pad.Size = New-Object System.Drawing.Size(420, 220)
$script:moves = 0
$pad.Add_MouseDown({ $script:moves = 0; Note "down $($_.Button) $($_.X),$($_.Y)" })
$pad.Add_MouseMove({ if ($_.Button -ne 'None') { $script:moves++ } })
$pad.Add_MouseUp({ Note "up $($_.Button) $($_.X),$($_.Y) moves=$($script:moves)" })
$pad.Add_MouseDoubleClick({ Note "double $($_.X),$($_.Y)" })
$form.Controls.Add($pad)

$form.Add_Shown({ Note 'ready' })
[System.Windows.Forms.Application]::Run($form)
