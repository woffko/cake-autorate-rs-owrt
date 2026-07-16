# SPDX-License-Identifier: Apache-2.0

include $(TOPDIR)/rules.mk

PKG_NAME:=luci-app-cake-autorate-rs
PKG_VERSION:=1.0_rc19
PKG_RELEASE:=1

PKG_MAINTAINER:=woffko
PKG_LICENSE:=Apache-2.0

include $(INCLUDE_DIR)/package.mk

define Package/luci-app-cake-autorate-rs
  SECTION:=luci
  CATEGORY:=LuCI
  SUBMENU:=3. Applications
  TITLE:=LuCI support for CAKE Autorate RS
  URL:=https://github.com/woffko/cake-autorate-rs-owrt
  DEPENDS:=+cake-autorate-rs +luci-base +sqm-scripts +uclient-fetch +jsonfilter +nftables-json
  PROVIDES:=luci-app-sqm
  CONFLICTS:=luci-app-sqm
  PKGARCH:=all
endef

define Package/luci-app-cake-autorate-rs/description
  LuCI support for CAKE Autorate RS with integrated SQM configuration.
endef

define Package/luci-app-cake-autorate-rs/postinst
[ -n "$${IPKG_INSTROOT}" ] || { \
	rm -f /tmp/luci-indexcache.*; \
	rm -rf /tmp/luci-modulecache/; \
	/etc/init.d/rpcd reload 2>/dev/null; \
	/etc/init.d/cake-autorate-autotune enable >/dev/null 2>&1 || true; \
	/etc/init.d/cake-autorate-autotune start >/dev/null 2>&1 || true; \
	exit 0; \
}
endef

define Build/Compile
endef

define Package/luci-app-cake-autorate-rs/install
	$(INSTALL_DIR) $(1)/www
	$(CP) ./htdocs/* $(1)/www/
	$(INSTALL_DIR) $(1)/
	$(CP) ./root/* $(1)/
endef

$(eval $(call BuildPackage,luci-app-cake-autorate-rs))
