//! WebView2 calls wry doesn't wrap: history state, load failures, and DevTools Protocol commands.

use webview2_com::Microsoft::Web::WebView2::Win32::{
    COREWEBVIEW2_BROWSING_DATA_KINDS, COREWEBVIEW2_BROWSING_DATA_KINDS_COOKIES,
    COREWEBVIEW2_BROWSING_DATA_KINDS_DISK_CACHE, COREWEBVIEW2_WEB_ERROR_STATUS, ICoreWebView2_13,
    ICoreWebView2Profile2,
};
use webview2_com::{
    CallDevToolsProtocolMethodCompletedHandler, ClearBrowsingDataCompletedHandler, HistoryChangedEventHandler,
    NavigationCompletedEventHandler, take_pwstr,
};
use windows_core::{BOOL, HSTRING, Interface as _, PWSTR};
use wry::WebViewExtWindows as _;

use super::PageEvent;

/// Whether the page can go back and forward.
pub(super) fn history(view: &wry::WebView) -> (bool, bool) {
    let webview = view.webview();
    let (mut back, mut forward) = (BOOL::default(), BOOL::default());
    // SAFETY: plain getters on a live ICoreWebView2, called on its UI thread.
    unsafe {
        let _ = webview.CanGoBack(&mut back);
        let _ = webview.CanGoForward(&mut forward);
    }
    (back.as_bool(), forward.as_bool())
}

/// Reports in-page history changes and failed navigations, which wry's load handler can't tell apart from success.
pub(super) fn watch(view: &wry::WebView, events: async_channel::Sender<PageEvent>) {
    let webview = view.webview();
    let history_events = events.clone();
    let on_history = HistoryChangedEventHandler::create(Box::new(move |_, _| {
        let _ = history_events.try_send(PageEvent::History);
        Ok(())
    }));
    let on_completed = NavigationCompletedEventHandler::create(Box::new(move |webview, args| {
        let (Some(webview), Some(args)) = (webview, args) else { return Ok(()) };
        let mut success = BOOL::default();
        let mut status = COREWEBVIEW2_WEB_ERROR_STATUS::default();
        let mut uri = PWSTR::null();
        // SAFETY: event args and sender are valid for the callback's duration.
        unsafe {
            args.IsSuccess(&mut success)?;
            if success.as_bool() {
                return Ok(());
            }
            args.WebErrorStatus(&mut status)?;
            webview.Source(&mut uri)?;
        }
        let url = take_pwstr(uri);
        if let Some((code, description)) = crate::preview::web_error(status.0) {
            let _ = events.try_send(PageEvent::Failed { url, code, description: description.into() });
        }
        Ok(())
    }));
    let mut token = 0i64;
    // SAFETY: registering handlers on a live ICoreWebView2 from its UI thread.
    unsafe {
        let _ = webview.add_HistoryChanged(&on_history, &mut token);
        let _ = webview.add_NavigationCompleted(&on_completed, &mut token);
    }
}

/// Sends one Chrome DevTools Protocol command to the page, ignoring its result.
pub(super) fn devtools_command(view: &wry::WebView, method: &str, params: &str) {
    let handler = CallDevToolsProtocolMethodCompletedHandler::create(Box::new(|_, _| Ok(())));
    // SAFETY: a live ICoreWebView2 on its UI thread; the strings outlive the call.
    unsafe {
        let _ = view.webview().CallDevToolsProtocolMethod(&HSTRING::from(method), &HSTRING::from(params), &handler);
    }
}

/// Clears the view's profile's cookies and HTTP cache, as Electron's partition `clearCookies` and `clearCache` do.
pub(super) async fn clear_cookies_and_cache(view: &wry::WebView) -> Result<(), String> {
    let (sender, done) = async_channel::bounded(1);
    let handler = ClearBrowsingDataCompletedHandler::create(Box::new(move |result| {
        let _ = sender.try_send(result);
        Ok(())
    }));
    let kinds = COREWEBVIEW2_BROWSING_DATA_KINDS(
        COREWEBVIEW2_BROWSING_DATA_KINDS_COOKIES.0 | COREWEBVIEW2_BROWSING_DATA_KINDS_DISK_CACHE.0,
    );
    // SAFETY: a live ICoreWebView2 on its UI thread; the handler outlives the call.
    unsafe {
        view.webview()
            .cast::<ICoreWebView2_13>()
            .and_then(|webview| webview.Profile())
            .and_then(|profile| profile.cast::<ICoreWebView2Profile2>())
            .and_then(|profile| profile.ClearBrowsingData(kinds, &handler))
            .map_err(|error| error.to_string())?;
    }
    done.recv().await.map_err(|error| error.to_string())?.map_err(|error| error.to_string())
}
