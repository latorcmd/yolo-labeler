# Original geometric artwork. Regenerates the PNG preview and multi-size ICO.
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
$assets = Join-Path $PSScriptRoot '..\crates\labeler-desktop\assets'
New-Item -ItemType Directory -Path $assets -Force | Out-Null
$canvas = [Drawing.Bitmap]::new(256, 256)
$graphics = [Drawing.Graphics]::FromImage($canvas)
$graphics.SmoothingMode = [Drawing.Drawing2D.SmoothingMode]::AntiAlias
$background = [Drawing.SolidBrush]::new([Drawing.ColorTranslator]::FromHtml('#FFFFFF'))
$accent = [Drawing.SolidBrush]::new([Drawing.ColorTranslator]::FromHtml('#147D72'))
$mountain = [Drawing.SolidBrush]::new([Drawing.ColorTranslator]::FromHtml('#91A9B8'))
$outline = [Drawing.Pen]::new([Drawing.ColorTranslator]::FromHtml('#253D50'), 10)
$edge = [Drawing.Pen]::new([Drawing.ColorTranslator]::FromHtml('#D6E3E9'), 2)
$corners = [Drawing.Pen]::new($accent, 12)
$path = [Drawing.Drawing2D.GraphicsPath]::new()
foreach ($arc in @(@(8,8,180),@(184,8,270),@(184,184,0),@(8,184,90))) {
    $path.AddArc($arc[0],$arc[1],64,64,$arc[2],90)
}
$path.CloseFigure()
$graphics.FillPath($background, $path)
$graphics.DrawPath($edge, $path)
$graphics.DrawRectangle($outline, 48, 61, 160, 134)
$points = [Drawing.PointF[]]@([Drawing.PointF]::new(55,187),[Drawing.PointF]::new(104,126),[Drawing.PointF]::new(137,160),[Drawing.PointF]::new(161,137),[Drawing.PointF]::new(201,187))
$graphics.FillPolygon($mountain, $points)
$graphics.FillEllipse($accent, 158, 83, 25, 25)
foreach ($segments in @(@(78,45,37,45,37,86),@(178,45,219,45,219,86),@(37,170,37,211,78,211),@(178,211,219,211,219,170))) {
    $graphics.DrawLine($corners,$segments[0],$segments[1],$segments[2],$segments[3])
    $graphics.DrawLine($corners,$segments[2],$segments[3],$segments[4],$segments[5])
}
$graphics.Dispose()
$canvas.Save((Join-Path $assets 'app.png'),[Drawing.Imaging.ImageFormat]::Png)
$sizes = @(16,24,32,48,64,128,256)
$images = @()
foreach ($size in $sizes) {
    $bitmap = [Drawing.Bitmap]::new($size,$size)
    $g = [Drawing.Graphics]::FromImage($bitmap)
    $g.InterpolationMode = [Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $g.DrawImage($canvas,0,0,$size,$size)
    $stream = [IO.MemoryStream]::new()
    $bitmap.Save($stream,[Drawing.Imaging.ImageFormat]::Png)
    $images += ,$stream.ToArray()
    $stream.Dispose(); $g.Dispose(); $bitmap.Dispose()
}
$file = [IO.File]::Create((Join-Path $assets 'app.ico'))
$writer = [IO.BinaryWriter]::new($file)
$writer.Write([uint16]0); $writer.Write([uint16]1); $writer.Write([uint16]$sizes.Count)
$offset = 6 + 16 * $sizes.Count
for ($i=0; $i -lt $sizes.Count; $i++) {
    $dimension = if($sizes[$i] -eq 256){0}else{$sizes[$i]}
    $writer.Write([byte]$dimension); $writer.Write([byte]$dimension)
    $writer.Write([byte]0); $writer.Write([byte]0)
    $writer.Write([uint16]1); $writer.Write([uint16]32)
    $writer.Write([uint32]$images[$i].Length); $writer.Write([uint32]$offset)
    $offset += $images[$i].Length
}
foreach($bytes in $images){$writer.Write([byte[]]$bytes)}
$writer.Dispose(); $file.Dispose(); $canvas.Dispose()
$background.Dispose(); $accent.Dispose(); $mountain.Dispose(); $outline.Dispose(); $edge.Dispose(); $corners.Dispose(); $path.Dispose()
