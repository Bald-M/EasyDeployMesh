export default defineNuxtConfig({
  compatibilityDate: '2026-07-01',
  devtools: { enabled: true },
  telemetry: false,
  ssr: false,
  spaLoadingTemplate: '../spa-loading-template.html',

  modules: [
    '@nuxt/ui',
    '@pinia/nuxt',
    '@nuxtjs/i18n'
  ],

  css: ['~/assets/css/main.css'],

  app: {
    head: {
      title: 'EasyDeployMesh',
      meta: [
        {
          name: 'description',
          content: 'Cross-platform Windows deployment orchestration'
        }
      ]
    }
  },

  i18n: {
    strategy: 'no_prefix',
    defaultLocale: 'zh-CN',
    detectBrowserLanguage: false,
    locales: [
      {
        code: 'zh-CN',
        language: 'zh-CN',
        name: '简体中文',
        file: 'zh-CN.json'
      },
      {
        code: 'en-US',
        language: 'en-US',
        name: 'English',
        file: 'en-US.json'
      }
    ]
  },

  vite: {
    clearScreen: false,
    envPrefix: ['VITE_', 'TAURI_'],
    server: {
      strictPort: true
    }
  },

  devServer: {
    host: '127.0.0.1',
    port: 3000
  },

  ignore: ['**/src-tauri/**'],

  typescript: {
    strict: true,
    typeCheck: true
  }
})
