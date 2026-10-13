using System.Runtime.InteropServices.WindowsRuntime;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media.Imaging;
using QRCoder;
using Windows.ApplicationModel.DataTransfer;
using Windows.Storage.Streams;

namespace ReinRemote.Host;

/// <summary>Small things the pages share: results in an InfoBar, confirmations, the clipboard, QR codes.</summary>
static class Ui
{
    /// <summary>An action's outcome: the host's error (red), or what it did (green), or nothing (hidden).</summary>
    public static void Result(InfoBar bar, string? error, string? done = null)
    {
        if (error is not null)
        {
            bar.Severity = InfoBarSeverity.Error;
            bar.Title = "Not done";
            bar.Message = error;
            bar.IsOpen = true;
        }
        else if (done is not null)
        {
            bar.Severity = InfoBarSeverity.Success;
            bar.Title = "Done";
            bar.Message = done;
            bar.IsOpen = true;
        }
        else
        {
            bar.IsOpen = false;
        }
    }

    public static void Copy(string text)
    {
        var data = new DataPackage();
        data.SetText(text);
        Clipboard.SetContent(data);
    }

    /// <summary>Asks first. With <paramref name="reason"/>, a box for a reason (returned; "" when left empty).</summary>
    public static async Task<(bool Yes, string Reason)> Confirm(XamlRoot root, string title, string text, string yes, bool reason = false)
    {
        var box = new TextBox
        {
            PlaceholderText = "A reason, shown to them (optional)",
            Margin = new Thickness(0, 12, 0, 0),
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(box, "ConfirmReason");
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(box, "Reason");
        var body = new StackPanel();
        body.Children.Add(new TextBlock { Text = text, TextWrapping = TextWrapping.Wrap });
        if (reason)
            body.Children.Add(box);
        var dialog = new ContentDialog
        {
            XamlRoot = root,
            Title = title,
            Content = body,
            PrimaryButtonText = yes,
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Close,
            RequestedTheme = (root.Content as FrameworkElement)?.ActualTheme ?? ElementTheme.Default,
        };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(dialog, "ConfirmDialog");
        var r = await dialog.ShowAsync();
        return (r == ContentDialogResult.Primary, box.Text.Trim());
    }

    static readonly Dictionary<string, BitmapImage> qrCache = new();

    /// <summary>A QR code of <paramref name="text"/>, black on white (scanners need the contrast, in either theme).</summary>
    public static async Task<BitmapImage?> Qr(string? text)
    {
        if (string.IsNullOrEmpty(text))
            return null;
        if (qrCache.TryGetValue(text, out var cached))
            return cached;
        using var gen = new QRCodeGenerator();
        using var data = gen.CreateQrCode(text, QRCodeGenerator.ECCLevel.M);
        var png = new PngByteQRCode(data).GetGraphic(8);
        using var stream = new InMemoryRandomAccessStream();
        await stream.WriteAsync(png.AsBuffer());
        stream.Seek(0);
        var image = new BitmapImage();
        await image.SetSourceAsync(stream);
        if (qrCache.Count > 32)
            qrCache.Clear();
        qrCache[text] = image;
        return image;
    }
}
