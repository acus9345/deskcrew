# A throwaway WPF window with an ink canvas. WPF takes no posted mouse messages, so deskcrew
# reaches it with its pen. Each stroke the canvas collects is logged.
param([string]$Title, [string]$Log, [int]$X = 300, [int]$Y = 300)
Add-Type -AssemblyName PresentationFramework
function Note([string]$text) { [System.IO.File]::AppendAllText($Log, "$text`n") }
$window = New-Object System.Windows.Window
$window.Title = $Title; $window.Width = 600; $window.Height = 450; $window.Left = $X; $window.Top = $Y
$window.WindowStartupLocation = 'Manual'; $window.ShowActivated = $false
$ink = New-Object System.Windows.Controls.InkCanvas
$ink.Background = 'White'
$ink.Add_StrokeCollected({ Note "stroke $($_.Stroke.StylusPoints.Count)" })
$window.Content = $ink
$window.Add_ContentRendered({ Note 'ready' })
[void]$window.ShowDialog()
