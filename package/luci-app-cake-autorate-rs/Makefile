# SPDX-License-Identifier: Apache-2.0

include $(TOPDIR)/rules.mk
ifeq ($(DISABLE_FAKEROOT),1)
override FAKEROOT :=
endif

PKG_NAME:=luci-app-cake-autorate-rs
PKG_VERSION:=1.0_rc27
PKG_RELEASE:=85

PKG_MAINTAINER:=woffko
PKG_LICENSE:=Apache-2.0

include $(INCLUDE_DIR)/package.mk

define Package/luci-app-cake-autorate-rs
  SECTION:=luci
  CATEGORY:=LuCI
  SUBMENU:=3. Applications
  TITLE:=LuCI support for CAKE Autorate RS
  URL:=https://github.com/woffko/cake-autorate-rs-owrt
  DEPENDS:=+cake-autorate-rs-full +luci-base +sqm-scripts +uclient-fetch +jsonfilter +nftables-json
  PROVIDES:=luci-app-cake-autorate-rs-full luci-app-sqm
  CONFLICTS:=luci-app-cake-autorate-rs-lite luci-app-sqm
  PKGARCH:=all
endef

define Package/luci-app-cake-autorate-rs/description
  LuCI support for CAKE Autorate RS with integrated SQM configuration.
endef

define Package/luci-app-cake-autorate-rs/postinst
[ -n "$${IPKG_INSTROOT}" ] || { \
	rm -f /tmp/luci-indexcache /tmp/luci-indexcache.*; \
	rm -rf /tmp/luci-modulecache /tmp/luci-modulecache.*; \
	/etc/init.d/rpcd reload >/dev/null 2>&1 || true; \
	/etc/init.d/cake-autorate-apply-guard disable >/dev/null 2>&1 || true; \
	/etc/init.d/cake-autorate-autotune enable >/dev/null 2>&1 || true; \
	/etc/init.d/cake-autorate-autotune start >/dev/null 2>&1 || true; \
	exit 0; \
}
endef

define Package/luci-app-cake-autorate-rs/prerm
[ -n "$${IPKG_INSTROOT}" ] || { \
	/etc/init.d/cake-autorate-autotune stop >/dev/null 2>&1 || true; \
	/etc/init.d/cake-autorate-autotune disable >/dev/null 2>&1 || true; \
	/etc/init.d/cake-autorate-apply-guard stop >/dev/null 2>&1 || true; \
	/etc/init.d/cake-autorate-apply-guard disable >/dev/null 2>&1 || true; \
	rm -f /etc/rc.d/S*cake-autorate-autotune /etc/rc.d/K*cake-autorate-autotune; \
	rm -f /etc/rc.d/S*cake-autorate-apply-guard /etc/rc.d/K*cake-autorate-apply-guard; \
}
endef

define Build/Compile
endef

define Package/luci-app-cake-autorate-rs/install
	$(INSTALL_DIR) $(1)/www
	$(CP) ./htdocs/* $(1)/www/
	$(INSTALL_DIR) $(1)/
	$(CP) ./root/* $(1)/
	$(RM) $(1)/usr/libexec/cake-autorate-rs/quality-test
endef

$(eval $(call BuildPackage,luci-app-cake-autorate-rs))
