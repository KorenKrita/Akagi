// Purchasable API-key products. Per the inference server's API documentation,
// there is no list-products endpoint — product ids are operator-defined
// constants shipped with the app, and the prices here are DISPLAY-ONLY (the
// server owns the real amount and re-verifies what PayPal captured; only the
// id crosses the wire). Update this table when the operator's catalog changes.

export type PurchaseKind = 'onetime' | 'subscription'

export type Product = {
  /** Operator-defined id sent to `create-order` / `create-subscription`. */
  id: string
  kind: PurchaseKind
  /** Plan the product grants — matches `GET /v3/key`'s `plan`. */
  plan: string
  /** i18n key for the product name (term / cycle). */
  nameKey: string
  /** Display-only price tag, currency included, e.g. `US$10`. */
  price: string
  /** i18n key for the billing suffix (`per month`, `one-time`, …). */
  priceSuffixKey: string
}

export const PRODUCTS: Product[] = [
  {
    id: 'pro-30',
    kind: 'onetime',
    plan: 'pro',
    nameKey: 'bots.api.buy_p_30d',
    price: 'US$10',
    priceSuffixKey: 'bots.api.buy_onetime',
  },
  {
    id: 'pro-90',
    kind: 'onetime',
    plan: 'pro',
    nameKey: 'bots.api.buy_p_90d',
    price: 'US$27',
    priceSuffixKey: 'bots.api.buy_onetime',
  },
  {
    id: 'pro-monthly',
    kind: 'subscription',
    plan: 'pro',
    nameKey: 'bots.api.buy_p_monthly',
    price: 'US$10',
    priceSuffixKey: 'bots.api.buy_per_month',
  },
  {
    id: 'pro-yearly',
    kind: 'subscription',
    plan: 'pro',
    nameKey: 'bots.api.buy_p_yearly',
    price: 'US$100',
    priceSuffixKey: 'bots.api.buy_per_year',
  },
]
