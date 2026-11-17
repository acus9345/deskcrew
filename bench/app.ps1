# Throwaway windows for the agent benchmark. Each records what reaches it in a log as JSON lines.
param([ValidateSet('order', 'stock')][string]$Task, [string]$Log, [int]$Seed = 7)
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
[System.Windows.Forms.Application]::EnableVisualStyles()
function Note($what) { [System.IO.File]::AppendAllText($Log, (ConvertTo-Json $what -Compress) + "`n") }
$random = New-Object System.Random($Seed)
$form = New-Object System.Windows.Forms.Form
$form.StartPosition = 'Manual'
$form.Location = New-Object System.Drawing.Point(120, 80)
$font = New-Object System.Drawing.Font('Segoe UI', 10)
$form.Font = $font

function Label([string]$text, [int]$x, [int]$y) {
    $l = New-Object System.Windows.Forms.Label
    $l.Text = $text; $l.Location = New-Object System.Drawing.Point($x, $y); $l.AutoSize = $true
    $form.Controls.Add($l); $l
}
function Box([string]$name, [int]$x, [int]$y, [int]$w) {
    $b = New-Object System.Windows.Forms.TextBox
    $b.Name = $name; $b.AccessibleName = $name; $b.Location = New-Object System.Drawing.Point($x, $y); $b.Width = $w
    $form.Controls.Add($b); $b
}

if ($Task -eq 'order') {
    $form.Text = 'Bench Order'
    $form.ClientSize = New-Object System.Drawing.Size(520, 420)
    [void](Label 'Name' 20 22); $name = Box 'Name' 140 20 340
    [void](Label 'Email' 20 62); $email = Box 'Email' 140 60 340
    [void](Label 'Quantity' 20 102)
    $qty = New-Object System.Windows.Forms.NumericUpDown
    $qty.Name = 'Quantity'; $qty.AccessibleName = 'Quantity'; $qty.Location = New-Object System.Drawing.Point(140, 100); $qty.Minimum = 1; $qty.Maximum = 99; $qty.Value = 1
    $form.Controls.Add($qty)
    [void](Label 'Country' 20 142)
    $country = New-Object System.Windows.Forms.ComboBox
    $country.Name = 'Country'; $country.AccessibleName = 'Country'; $country.DropDownStyle = 'DropDownList'; $country.Location = New-Object System.Drawing.Point(140, 140); $country.Width = 200
    [void]$country.Items.AddRange(@('Brazil', 'Canada', 'France', 'Germany', 'India', 'Japan', 'Kenya', 'Mexico', 'Peru', 'Spain'))
    $country.SelectedIndex = 1
    $form.Controls.Add($country)
    $ship = New-Object System.Windows.Forms.GroupBox
    $ship.Text = 'Shipping'; $ship.Location = New-Object System.Drawing.Point(20, 180); $ship.Size = New-Object System.Drawing.Size(460, 60)
    $standard = New-Object System.Windows.Forms.RadioButton; $standard.Text = 'Standard'; $standard.Location = New-Object System.Drawing.Point(16, 24); $standard.Checked = $true
    $express = New-Object System.Windows.Forms.RadioButton; $express.Text = 'Express'; $express.Location = New-Object System.Drawing.Point(160, 24)
    $ship.Controls.Add($standard); $ship.Controls.Add($express)
    $form.Controls.Add($ship)
    $gift = New-Object System.Windows.Forms.CheckBox
    $gift.Text = 'Gift wrap'; $gift.Location = New-Object System.Drawing.Point(20, 254)
    $form.Controls.Add($gift)
    $status = Label 'No order yet' 20 340
    $status.Name = 'Status'
    $submit = New-Object System.Windows.Forms.Button
    $submit.Text = 'Submit order'; $submit.Location = New-Object System.Drawing.Point(20, 292); $submit.Width = 140
    $submit.Add_Click({
        $answer = [System.Windows.Forms.MessageBox]::Show($form, "Place this order for $($qty.Value) item(s)?", 'Confirm order', 'YesNo', 'Question')
        if ($answer -ne 'Yes') { Note @{ event = 'declined' }; return }
        $number = $random.Next(10000, 99999)
        $status.Text = "Order #$number placed"
        Note @{ event = 'order'; number = $number; name = $name.Text; email = $email.Text; quantity = [int]$qty.Value
                country = [string]$country.SelectedItem; shipping = $(if ($express.Checked) { 'Express' } else { 'Standard' }); gift = $gift.Checked }
    })
    $form.Controls.Add($submit)
}

if ($Task -eq 'stock') {
    $form.Text = 'Bench Stock'
    $form.ClientSize = New-Object System.Drawing.Size(560, 520)
    $grid = New-Object System.Windows.Forms.DataGridView
    $grid.Name = 'Stock'; $grid.AccessibleName = 'Stock'
    $grid.Location = New-Object System.Drawing.Point(20, 20); $grid.Size = New-Object System.Drawing.Size(520, 360)
    $grid.ReadOnly = $true; $grid.AllowUserToAddRows = $false; $grid.RowHeadersVisible = $false
    [void]$grid.Columns.Add('Item', 'Item'); [void]$grid.Columns.Add('Price', 'Price'); [void]$grid.Columns.Add('Stock', 'Stock')
    $grid.Columns[0].Width = 220
    $words = @('Anchor', 'Bolt', 'Clamp', 'Dowel', 'Eyelet', 'Flange', 'Gasket', 'Hinge')
    $lowest = $null
    foreach ($i in 1..80) {
        $item = '{0} {1:D2}' -f $words[($i - 1) % 8], $i
        $price = '{0:N2}' -f ($random.Next(100, 9999) / 100)
        $stock = $random.Next(12, 400)
        if ($i -eq 67) { $stock = 3 }
        [void]$grid.Rows.Add($item, $price, $stock)
        if ($null -eq $lowest -or $stock -lt $lowest[1]) { $lowest = @($item, $stock) }
    }
    $form.Controls.Add($grid)
    [void](Label 'Answer' 20 402); $answer = Box 'Answer' 100 400 240
    $result = Label 'Not checked' 20 470
    $result.Name = 'Result'
    $check = New-Object System.Windows.Forms.Button
    $check.Text = 'Check'; $check.Location = New-Object System.Drawing.Point(360, 398); $check.Width = 100
    $check.Add_Click({
        $right = $answer.Text.Trim() -eq $lowest[0]
        $result.Text = $(if ($right) { 'Correct' } else { 'Wrong' })
        Note @{ event = 'check'; answer = $answer.Text; right = $right }
    })
    $form.Controls.Add($check)
}

$form.Add_Shown({ Note @{ event = 'ready' } })
[System.Windows.Forms.Application]::Run($form)
