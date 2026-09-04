const invoke = (command, args = {}) => window.__TAURI__.core.invoke(command, args);

export const getWidgetView = () => invoke("get_widget_view");
export const onRefreshCompleted = (handler) => window.__TAURI__.event.listen(
  "usage-refresh-completed", (event) => handler(event.payload)
);
export const hideWidget = () => invoke("hide_widget");
export const setWidgetHeight = (height) => invoke("set_widget_height", {height});
