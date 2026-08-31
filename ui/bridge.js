const invoke = (command, args = {}) => window.__TAURI__.core.invoke(command, args);

export const getWidgetView = () => invoke("get_widget_view");
export const refresh = () => invoke("refresh");
export const hideWidget = () => invoke("hide_widget");
export const setWidgetLayout = (layout) => invoke("set_widget_layout", {layout});
