'use strict';
// Match the server-negotiated document language, including TUI reservations.
const webminLanguage=document.documentElement.lang;
function t(source,values={}) {
  const translated=WEBMIN_TRANSLATIONS[source]?.[webminLanguage]||source;
  return translated.replace(/\{([a-z]+)\}/g,(match,key)=>Object.hasOwn(values,key)?String(values[key]):match);
}
