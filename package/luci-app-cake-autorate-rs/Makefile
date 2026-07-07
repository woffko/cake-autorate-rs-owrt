# SPDX-License-Identifier: Apache-2.0

include $(TOPDIR)/rules.mk

PKG_MAINTAINER:=woffko
PKG_LICENSE:=Apache-2.0

LUCI_TITLE:=LuCI support for CAKE Autorate RS
LUCI_DEPENDS:=+cake-autorate-rs +luci-base
LUCI_PKGARCH:=all

include $(TOPDIR)/feeds/luci/luci.mk

$(eval $(call BuildPackage,luci-app-cake-autorate-rs))
